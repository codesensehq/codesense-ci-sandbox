// SPDX-License-Identifier: MIT
//! Seam tests for the routine MCP tools: each body is driven with the
//! parameters as an agent sends them (JSON through the tool's own params
//! type), against a real temp home, coordinator and scheduler, and its reply is
//! checked as the envelope the agent reads.
//!
//! The `#[tool]` methods themselves need the desktop runtime; what they add on
//! top of these bodies (who the caller is, which team) is covered by the
//! handler's registration tests.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::json;

use super::*;
use crate::context_repo::{path_to_org_root, path_to_team_config_dir};
use crate::domain::Principal;
use crate::routines::defs;
use crate::routines::service::PersonalOwner;
use crate::routines::test_support::{
    agent as test_agent, alice, drain_claim_loop, save_personal_agent, save_team_agent, stub_agent,
    write_org, StubLauncher, TempHome, AGENT, ORG, TEAM,
};
use crate::routines::tick::{Clock, RoutineAdmitter, RoutineTicker};
use codesense_pty_core::agent_cli::AgentCli;

#[test]
fn routine_result_payload_schema_is_client_compatible_without_restricting_json() {
    let schema =
        serde_json::to_value(rmcp::schemars::schema_for!(RoutinePublishResultParams)).unwrap();
    assert_eq!(schema["properties"]["payload"], json!({}));
    for payload in [
        json!(null),
        json!(true),
        json!(42),
        json!("text"),
        json!([]),
        json!({"count": 9}),
    ] {
        let input = json!({"sessionId": "session", "payload": payload});
        let decoded: RoutinePublishResultParams = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(decoded.payload, payload);
        assert!(jsonschema::is_valid(&schema, &input));
    }
    assert!(!jsonschema::is_valid(
        &schema,
        &json!({"sessionId": "session"})
    ));
}

fn at(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

fn team() -> Placement {
    Placement::Team {
        org: ORG.into(),
        team: TEAM.into(),
    }
}

/// The arguments as an agent sends them.
fn params<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
    serde_json::from_value(value).expect("the agent's arguments parse")
}

/// The envelope as the agent reads it.
fn wire<T: Serialize>(reply: Reply<T>) -> serde_json::Value {
    serde_json::to_value(reply.expect("not a hard failure")).unwrap()
}

fn ok<T: Serialize>(reply: Reply<T>) -> serde_json::Value {
    let wire = wire(reply);
    assert_eq!(wire["status"], "ok", "{wire}");
    wire["data"].clone()
}

fn ok_wire(wire: serde_json::Value) -> serde_json::Value {
    assert_eq!(wire["status"], "ok", "{wire}");
    wire["data"].clone()
}

fn create_echo(home: &Path, extra: serde_json::Value, now: DateTime<Utc>) -> serde_json::Value {
    let mut args = json!({
        "name": "echo", "task": "echo hi", "when": "every 5m", "tz": "UTC", "agent": AGENT
    });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    wire(create(
        home,
        &alice(),
        team(),
        &CallerSession::default(),
        &CallerSession::default().author(),
        params(args),
        now,
    ))
}

fn ticker(home: &Path, clock: &Arc<AtomicI64>, launcher: Arc<StubLauncher>) -> RoutineTicker {
    let c = Arc::clone(clock);
    let clock: Clock = Arc::new(move || c.load(Ordering::SeqCst));
    let home = home.to_path_buf();
    RoutineTicker::new(
        clock,
        Arc::new(RoutineAdmitter),
        Arc::new(move || Some((home.clone(), alice()))),
        launcher,
    )
}

fn files_under(root: &Path) -> BTreeSet<PathBuf> {
    fn walk(dir: &Path, out: &mut BTreeSet<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.insert(path);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, &mut out);
    out
}

fn runs_of(home: &Path, id: &str) -> serde_json::Value {
    ok(runs(home, &alice(), params(json!({ "id": id }))))
}

/// Create a routine, see it listed with its next fires, let the scheduler
/// fire it and read the run back: the whole agent loop through the real
/// service, scheduler and coordinator.
#[tokio::test]
async fn a_created_routine_is_listed_fired_and_its_run_read_back() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");

    let created = create_echo(&home, json!({}), now);
    assert_eq!(created["status"], "ok", "{created}");
    let data = &created["data"];
    let id = data["id"].as_str().unwrap().to_string();
    assert_eq!(id, format!("{ORG}/{TEAM}/echo"));
    assert_eq!(
        data["next_fires"],
        json!([
            "2026-10-05T09:05:00Z",
            "2026-10-05T09:10:00Z",
            "2026-10-05T09:15:00Z"
        ])
    );
    assert_eq!(data["enabled"], true);
    assert_eq!(data["revision"], 1);
    assert_eq!(data["agent"], AGENT, "the team agent it names");
    assert_eq!(data["warnings"], json!([]));
    assert!(data["schedule_in_words"].as_str().unwrap().contains('5'));
    assert!(data.get("dry_run").is_none());

    let listed = ok(list(&home, &alice(), Default::default(), now));
    let item = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id.as_str())
        .expect("listed");
    assert_eq!(item["next_run_at"], "2026-10-05T09:05:00Z");
    assert_eq!(item["where"], "team_workspace");
    assert_eq!(item["scope"], "team");
    assert_eq!(item["owner"], "1");
    assert_eq!(
        item["created_by"],
        json!({ "kind": "agent", "name": "mcp" })
    );
    assert_eq!(item["armed_here"], true);
    assert_eq!(item["last_run"], serde_json::Value::Null);

    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(now.timestamp_millis()));
    let ticker = ticker(&home, &clock, launcher);
    ticker.tick().unwrap();
    clock.store(
        at("2026-10-05T09:05:10Z").timestamp_millis(),
        Ordering::SeqCst,
    );
    assert_eq!(ticker.tick().unwrap().admitted, 1);

    let page = runs_of(&home, &id);
    let rows = page["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["phase"], "queued");
    assert_eq!(rows[0]["source"], "schedule");
    assert_eq!(rows[0]["created_at"], "2026-10-05T09:05:10Z");
    assert!(rows[0].get("session_id").is_none());
    assert_eq!(page["next_before"], serde_json::Value::Null);

    let got = ok(get(&home, &alice(), params(json!({ "id": id })), now));
    assert_eq!(got["revision"], 1);
    assert_eq!(got["routine"]["task"], "echo hi");
    assert_eq!(got["routine"]["policy"]["max_runtime"], "30m");
    assert_eq!(got["runs"].as_array().unwrap().len(), 1);
}

/// A draft is written but never fires.
#[test]
fn a_draft_is_saved_paused_and_the_scheduler_never_fires_it() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let data = ok_wire(create_echo(&home, json!({ "enabled": false }), now));
    assert_eq!(data["enabled"], false);
    let id = data["id"].as_str().unwrap().to_string();
    let detail = service::get(&home, &alice(), &RoutineKey::parse(&id).unwrap()).unwrap();
    assert!(!detail.enabled);

    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    let clock = Arc::new(AtomicI64::new(now.timestamp_millis()));
    let ticker = ticker(&home, &clock, launcher);
    ticker.tick().unwrap();
    clock.store(
        at("2026-10-05T09:05:10Z").timestamp_millis(),
        Ordering::SeqCst,
    );
    assert_eq!(ticker.tick().unwrap().admitted, 0);
    assert_eq!(runs_of(&home, &id)["runs"], json!([]));
}

/// Two edits from the same base: the second is a conflict and changes nothing.
#[test]
fn a_stale_update_is_a_conflict_that_changes_nothing() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({}), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let edit = |name: &str| {
        wire(update(
            &home,
            &alice(),
            &CallerSession::default().author(),
            params(json!({ "id": id, "base_revision": 1, "name": name })),
            now,
            &|_| None,
        ))
    };

    let first = edit("first");
    assert_eq!(first["status"], "ok", "{first}");
    assert_eq!(first["data"]["revision"], 2);

    let second = edit("second");
    assert_eq!(second["status"], "conflict");
    assert_eq!(second["data"]["current_revision"], 2);
    let detail = service::get(&home, &alice(), &RoutineKey::parse(&id).unwrap()).unwrap();
    assert_eq!(detail.def.name, "first");
    assert_eq!(detail.def.revision, 2);
}

/// A schedule change goes through the same `when` parsing, keeps the zone,
/// and reports the new fires.
#[test]
fn an_update_replaces_the_schedule_in_the_routines_zone() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({ "tz": "America/Denver" }), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let data = ok(update(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({ "id": id, "baseRevision": 1, "when": "daily 07:30" })),
        now,
        &|_| None,
    ));
    assert_eq!(data["next_fires"][0], "2026-10-05T13:30:00Z");
    let detail = service::get(&home, &alice(), &RoutineKey::parse(&id).unwrap()).unwrap();
    assert_eq!(
        detail.def.triggers,
        vec![RoutineTrigger::Schedule(ScheduleSpec {
            cron: "30 7 * * *".into(),
            tz: "America/Denver".into(),
        })]
    );
}

/// A teammate's agent manages a team routine like its creator's would: every
/// mutating tool goes through, and the creator stays its `owner`.
#[test]
fn a_teammates_agent_can_change_every_part_of_a_team_routine() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({}), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    // The git pull that brings Alice's team routine to Bob.
    let bob = Principal::from(2u64);
    let routine_dir = |who: &Principal| {
        defs::routine_dir(
            &path_to_team_config_dir(&path_to_org_root(&home, who, ORG).unwrap(), TEAM),
            "echo",
        )
    };
    write_org(&home, 2);
    let from = routine_dir(&alice());
    let to = crate::routines::defs::sync_target(&from, &routine_dir(&bob));
    std::fs::create_dir_all(&to).unwrap();
    for entry in std::fs::read_dir(&from).unwrap().filter_map(Result::ok) {
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }

    let caller = CallerSession::default();
    let updated = ok(update(
        &home,
        &bob,
        &caller.author(),
        params(json!({ "id": id, "baseRevision": 1, "name": "bob's" })),
        now,
        &|_| None,
    ));
    assert_eq!(updated["revision"], 2);
    // A stale base is still a conflict, for him as for anyone.
    let stale = wire(update(
        &home,
        &bob,
        &caller.author(),
        params(json!({ "id": id, "baseRevision": 1, "name": "again" })),
        now,
        &|_| None,
    ));
    assert_eq!(stale["status"], "conflict", "{stale}");

    let paused = ok(set_enabled(
        &home,
        &bob,
        params(json!({ "id": id, "enabled": false })),
    ));
    assert_eq!(paused["enabled"], false);
    ok(set_enabled(
        &home,
        &bob,
        params(json!({ "id": id, "enabled": true })),
    ));
    let fired = ok(run_now(&home, &bob, params(json!({ "id": id }))));
    assert_eq!(fired["outcome"], "queued", "{fired}");
    let cancelled = ok(cancel(
        &home,
        &bob,
        params(json!({"runId": fired["run_id"]})),
        &crate::routines::test_support::FakeSessions::default(),
    ));
    assert_eq!(cancelled["phase"], "cancelled");

    let detail = ok(get(&home, &bob, params(json!({ "id": id })), now));
    assert_eq!(
        (detail["owner"].as_str(), detail["revision"].as_u64()),
        (Some("1"), Some(2))
    );

    let deleted = ok(delete(&home, &bob, &caller, params(json!({ "id": id }))));
    assert_eq!(deleted["archived"], true);
    assert!(service::get(&home, &bob, &RoutineKey::parse(&id).unwrap())
        .unwrap()
        .def
        .is_archived());
}

/// A bad `when`, a too-frequent schedule and a bad runtime are `validation`
/// the agent can correct, and nothing is written.
#[test]
fn bad_inputs_are_validation_and_write_nothing() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    local_host_id(&home).unwrap();
    let before = files_under(temp.root());

    let unrecognized = create_echo(&home, json!({ "when": "every 7m" }), now);
    assert_eq!(unrecognized["status"], "validation");
    let message = unrecognized["data"]["message"].as_str().unwrap();
    assert!(
        message.contains("every 7m") && message.contains("weekdays"),
        "the error lists the accepted forms: {message}"
    );

    let too_often = create_echo(&home, json!({ "when": "*/2 * * * *" }), now);
    assert_eq!(too_often["status"], "validation", "{too_often}");

    let runtime = create_echo(&home, json!({ "max_runtime": "90s" }), now);
    assert_eq!(runtime["status"], "validation", "{runtime}");
    assert!(runtime["data"]["message"]
        .as_str()
        .unwrap()
        .contains("max_runtime"));

    let agent = create_echo(&home, json!({ "agent": "nobody" }), now);
    assert_eq!(agent["status"], "validation", "{agent}");

    let scope = wire(update(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({ "id": "not a key", "baseRevision": 1 })),
        now,
        &|_| None,
    ));
    assert_eq!(scope["status"], "validation");

    let written: Vec<_> = files_under(temp.root())
        .difference(&before)
        .filter(|p| p.to_string_lossy().contains("routines"))
        .cloned()
        .collect();
    assert_eq!(written, Vec::<PathBuf>::new());
}

/// A dry run reports what would be created and writes nothing at all.
#[test]
fn a_dry_run_reports_the_routine_and_writes_nothing() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    // The machine's host id is minted once, before the snapshot, like any
    // app that has run before.
    local_host_id(&home).unwrap();
    let before = files_under(temp.root());

    let data = ok_wire(create_echo(&home, json!({ "dry_run": true }), now));
    assert_eq!(data["dry_run"], true);
    assert_eq!(data["revision"], 1);
    assert_eq!(data["id"], format!("{ORG}/{TEAM}/echo"));
    assert_eq!(data["next_fires"].as_array().unwrap().len(), 3);
    assert_eq!(files_under(temp.root()), before);
    assert_eq!(
        ok(list(&home, &alice(), Default::default(), now)),
        json!([])
    );

    let invalid = create_echo(&home, json!({ "dryRun": true, "when": "*/1 * * * *" }), now);
    assert_eq!(invalid["status"], "validation");
}

/// Creating, changing and deleting a routine tells no one: the only files
/// written are the routine's own, the run coordinator's and nothing of
/// Weft's attention ledger.
#[test]
fn writes_raise_no_notice() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    local_host_id(&home).unwrap();
    let coordinator = crate::coordinator::default_store_path(&home);
    let coordinator_dir = coordinator.parent().unwrap().to_path_buf();
    let before = files_under(temp.root());

    let id = create_echo(&home, json!({}), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    ok(update(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({ "id": id, "baseRevision": 1, "task": "echo there" })),
        now,
        &|_| None,
    ));
    ok(delete(
        &home,
        &alice(),
        &CallerSession::default(),
        params(json!({ "id": id })),
    ));

    let routine_dir = defs::routine_dir(
        &path_to_team_config_dir(&path_to_org_root(&home, &alice(), ORG).unwrap(), TEAM),
        "echo",
    );
    let after = files_under(temp.root());
    let elsewhere: Vec<_> = after
        .difference(&before)
        .filter(|p| !p.starts_with(&routine_dir) && !p.starts_with(&coordinator_dir))
        .collect();
    assert!(elsewhere.is_empty(), "unexpected writes: {elsewhere:?}");
    assert!(
        !after.iter().any(|p| p.ends_with("weft.sqlite3")),
        "routines never open the Weft ledger"
    );
}

/// With no `agent`, a routine runs as the agent the calling session was
/// launched as. With neither, or with an agent the caller cannot see, the
/// create is `validation` naming agent_list; there is no launch profile
/// fallback.
#[test]
fn the_default_agent_is_the_agent_the_caller_was_launched_as() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let make = |name: &str, caller: CallerSession, agent: Option<&str>| {
        let mut args = json!({ "name": name, "task": "t", "when": "hourly" });
        if let Some(agent) = agent {
            args["agent"] = json!(agent);
        }
        wire(create(
            &home,
            &alice(),
            team(),
            &caller,
            &caller.author(),
            params(args),
            now,
        ))
    };
    let launched_as = |key: &AgentKey| CallerSession {
        profile_id: Some(key.to_string()),
        agent: Some(key.clone()),
        ..CallerSession::default()
    };

    let team_agent = AgentKey::new(ORG, TEAM, AGENT);
    let own = ok_wire(make("own", launched_as(&team_agent), None));
    assert_eq!(own["agent"], AGENT, "written as the team's own agent id");
    let key = RoutineKey::parse(own["id"].as_str().unwrap()).unwrap();
    assert_eq!(
        service::get(&home, &alice(), &key)
            .unwrap()
            .def
            .agent
            .as_str(),
        AGENT
    );

    // No agent named and none launched: name one.
    let none = make("none", CallerSession::default(), None);
    assert_eq!(none["status"], "validation", "{none}");
    assert_eq!(
        none["data"]["message"],
        "agent is required: a routine belongs to an agent; name one (see agent_list)"
    );

    // A personal agent cannot run a team routine, whoever was launched as it.
    let mine = save_personal_agent(&home, &alice(), test_agent("mine", AgentCli::Claude));
    let personal = make("personal", launched_as(&mine), None);
    assert_eq!(personal["status"], "validation", "{personal}");
    assert!(personal["data"]["message"]
        .as_str()
        .unwrap()
        .contains("a team routine needs a team agent"));

    // An agent nobody here can see (unknown, or another account's personal
    // one) is refused and points at agent_list.
    for hidden in [
        "nobody",
        "_personal/2/acme/api/_scratch/mine",
        "acme/api/backend/nobody",
    ] {
        let reply = wire(create(
            &home,
            &alice(),
            Placement::Personal(PersonalOwner {
                github_user_id: 1,
                username: "alice".into(),
                display_name: "Alice".into(),
                avatar_url: None,
                org: "acme/api".into(),
            }),
            &CallerSession::default(),
            &CallerSession::default().author(),
            params(json!({ "name": format!("h{}", hidden.len()), "task": "t", "agent": hidden })),
            now,
        ));
        assert_eq!(reply["status"], "validation", "{hidden}: {reply}");
        assert!(
            reply["data"]["message"]
                .as_str()
                .unwrap()
                .contains("agent_list"),
            "{reply}"
        );
    }
    // A `pack:id` spelling is an ordinary invalid agent.
    let profile = make("profile", CallerSession::default(), Some("claude:default"));
    assert_eq!(profile["status"], "validation", "{profile}");
    assert!(profile["data"]["message"]
        .as_str()
        .unwrap()
        .contains("is not an agent id or key; see agent_list"));
}

#[test]
fn run_now_reports_the_admission_outcome() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({}), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let first = ok(run_now(&home, &alice(), params(json!({ "id": id }))));
    assert_eq!(first["outcome"], "queued");
    let second = ok(run_now(&home, &alice(), params(json!({ "id": id }))));
    assert_eq!(second["outcome"], "coalesced");
    assert_eq!(second["active_run_id"], first["run_id"]);

    ok(set_enabled(
        &home,
        &alice(),
        params(json!({ "id": id, "enabled": false })),
    ));
    let paused = ok(run_now(&home, &alice(), params(json!({ "id": id }))));
    assert_eq!(paused["outcome"], "skipped:paused");

    let history = runs_of(&home, &id);
    let phases: Vec<_> = history["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["phase"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(phases, ["skipped", "coalesced", "queued"]);

    let unknown = wire(run_now(
        &home,
        &alice(),
        params(json!({ "id": id, "variables": { "nope": "1" } })),
    ));
    assert_eq!(unknown["status"], "validation");
}

/// `routine_run` reads one run by id, with its routine, in the same row shape
/// routine_runs lists; an unknown run is `validation`.
#[test]
fn one_run_is_read_back_by_its_id_with_its_routine() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({}), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let fired = ok(run_now(&home, &alice(), params(json!({ "id": id }))));
    let run_id = fired["run_id"].as_str().unwrap();

    let got = ok(run(&home, &alice(), params(json!({ "runId": run_id }))));
    assert_eq!(got["routine_id"], id.as_str());
    assert_eq!(got["routine_name"], "echo");
    let listed = runs_of(&home, &id)["runs"][0].clone();
    for (field, value) in listed.as_object().unwrap() {
        assert_eq!(
            &got[field], value,
            "routine_run `{field}` matches routine_runs"
        );
    }
    assert!(got.get("session_id").is_none());

    let unknown = wire(run(&home, &alice(), params(json!({ "runId": "nope" }))));
    assert_eq!(unknown["status"], "validation");
    let blank = wire(run(&home, &alice(), params(json!({ "run_id": " " }))));
    assert_eq!(blank["status"], "validation");
}

/// A run that could not start reads back with why in `detail` and no `result_text`, in both
/// routine_runs and routine_run.
#[test]
fn a_refused_run_reads_back_its_detail_and_no_result_text() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({}), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let fired = ok(run_now(&home, &alice(), params(json!({ "id": id }))));
    let run_id = fired["run_id"].as_str().unwrap();
    let store = crate::routines::state::RoutineStateStore::open(&home).unwrap();
    let refused = crate::routines::finish::Ending::Failed {
        reason: "launch_failed",
        detail: "the agent refused safe-mode".into(),
    };
    crate::routines::finish::finish(&store, run_id, refused, crate::routines::finish::now_ms())
        .unwrap()
        .expect("the queued run ends");
    let listed = runs_of(&home, &id)["runs"][0].clone();
    let got = ok(run(&home, &alice(), params(json!({ "runId": run_id }))));
    for row in [&listed, &got] {
        assert_eq!(row["detail"], "the agent refused safe-mode");
        assert!(row["result_text"].is_null());
    }
}

/// Delete archives: the routine leaves the list and its runs stay readable.
#[test]
fn delete_archives_and_keeps_history() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({}), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    ok(run_now(&home, &alice(), params(json!({ "id": id }))));
    let archived = ok(delete(
        &home,
        &alice(),
        &CallerSession::default(),
        params(json!({ "id": id })),
    ));
    assert_eq!(
        archived,
        json!({ "id": id, "archived": true, "revision": 2 })
    );
    assert_eq!(
        ok(list(&home, &alice(), Default::default(), now)),
        json!([])
    );
    assert_eq!(runs_of(&home, &id)["runs"].as_array().unwrap().len(), 1);
}

/// The list covers the personal space and every synced team.
#[test]
fn the_list_covers_personal_and_team_routines() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    create_echo(&home, json!({}), now);
    let personal = ok(create(
        &home,
        &alice(),
        Placement::Personal(PersonalOwner {
            github_user_id: 1,
            username: "alice".into(),
            display_name: "Alice".into(),
            avatar_url: None,
            org: "acme/api".into(),
        }),
        &CallerSession::default(),
        &CallerSession::default().author(),
        params(json!({
            "name": "Standup notes", "task": "t", "when": "weekdays 09:00",
            "agent": "acme/api/backend/default",
        })),
        now,
    ));
    assert_eq!(
        personal["id"],
        "_personal/1/acme/api/_scratch/standup-notes"
    );
    let listed = ok(list(&home, &alice(), Default::default(), now));
    let scopes: BTreeSet<_> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["scope"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(scopes, BTreeSet::from(["personal".into(), "team".into()]));
}

/// An agent CodeSense did not launch has no session id. It still sets up a
/// routine in one call with only `name`, `task` and `when`, and manages it
/// after, acting as the signed-in user: the caller a session-less call
/// resolves to is the default one, with no launch profile of its own.
#[test]
fn an_agent_without_a_session_sets_up_and_manages_a_routine() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let no_session = CallerSession::default();
    let created = ok(create(
        &home,
        &alice(),
        Placement::Personal(PersonalOwner {
            github_user_id: 1,
            username: "alice".into(),
            display_name: "Alice".into(),
            avatar_url: None,
            org: "acme/api".into(),
        }),
        &no_session,
        &no_session.author(),
        params(json!({
            "name": "Inbox sweep", "task": "sweep it", "when": "daily 09:00",
            "agent": "acme/api/backend/default",
        })),
        now,
    ));
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(id, "_personal/1/acme/api/_scratch/inbox-sweep");
    assert_eq!(
        created["agent"], "acme/api/backend/default",
        "a personal routine names a team agent by its key"
    );
    assert_eq!(created["enabled"], true);
    assert_eq!(created["next_fires"].as_array().unwrap().len(), 3);
    let key = RoutineKey::parse(&id).unwrap();
    let detail = service::get(&home, &alice(), &key).unwrap();
    assert_eq!(detail.def.owner, alice().as_str());
    assert_eq!(detail.revisions[0].author, Author::agent("mcp"));

    let updated = ok(update(
        &home,
        &alice(),
        &no_session.author(),
        params(json!({ "id": id, "baseRevision": 1, "when": "weekdays 08:30" })),
        now,
        &|_| None,
    ));
    assert_eq!(updated["revision"], 2);
    ok(set_enabled(
        &home,
        &alice(),
        params(json!({ "id": id, "enabled": false })),
    ));
    let fired = ok(run_now(&home, &alice(), params(json!({ "id": id }))));
    assert_eq!(fired["outcome"], "skipped:paused");
    ok(delete(
        &home,
        &alice(),
        &no_session,
        params(json!({ "id": id })),
    ));
}

/// Inside a routine run, the agent reads its pinned run context; anywhere
/// else it is told this is not a routine run.
#[tokio::test]
async fn routine_context_reads_the_pinned_run() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({}), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let run_id = ok(run_now(&home, &alice(), params(json!({ "id": id }))))["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Edited after the fire: the run stays pinned to revision 1.
    ok(update(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({ "id": id, "baseRevision": 1, "task": "echo later" })),
        now,
        &|_| None,
    ));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let session = launcher.last().session_id;

    let context = ok(context(&home, &session, None));
    assert_eq!(context["run_id"], run_id.as_str());
    assert_eq!(context["routine_id"], id.as_str());
    assert_eq!(context["revision"], 1);
    assert_eq!(context["routine"]["task"], "echo hi");
    assert_eq!(context["source"], "manual");
    assert!(context["fired_at"].as_str().unwrap().ends_with('Z'));

    assert_eq!(
        wire(super::context(&home, "shell-not-a-routine", None)),
        json!({ "status": "validation", "data": { "message": "not a routine run" } })
    );
    launcher.signal_done(&session, "success", "ok").await;
    launcher.exit(&session).await;
}

/// At the active-routine cap a draft can still be saved, since it never
/// fires, while an enabled routine and its dry run are refused alike.
#[test]
fn a_draft_does_not_count_toward_the_cap() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    for n in 0..service::MAX_ACTIVE_ROUTINES {
        ok_wire(create_echo(&home, json!({ "name": format!("r{n}") }), now));
    }
    let refused = create_echo(&home, json!({ "name": "one more" }), now);
    assert_eq!(refused["status"], "validation", "{refused}");
    let dry = create_echo(&home, json!({ "name": "one more", "dryRun": true }), now);
    assert_eq!(dry["status"], "validation", "{dry}");

    let draft = ok_wire(create_echo(
        &home,
        json!({ "name": "one more", "enabled": false }),
        now,
    ));
    assert_eq!(draft["enabled"], false);
    assert_eq!(
        draft["next_fires"],
        json!([]),
        "a draft has no upcoming fires"
    );

    // A draft the service refuses leaves no switch behind for its id.
    let bad = create_echo(
        &home,
        json!({ "name": "bad", "enabled": false, "maxRuntime": "90s" }),
        now,
    );
    assert_eq!(bad["status"], "validation", "{bad}");
    let switches = RoutineStateStore::open(&home).unwrap().switches().unwrap();
    assert!(switches
        .get(&RoutineKey::new(ORG, TEAM, "bad").to_string())
        .is_none_or(|s| s.enabled && s.mode == crate::routines::model::RunsOn::OneMac));
}

/// A blank `tz` on an update keeps the routine's own zone.
#[test]
fn a_blank_tz_keeps_the_routines_zone() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({ "tz": "America/Denver" }), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    ok(update(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({ "id": id, "baseRevision": 1, "when": "daily 10:00", "tz": "" })),
        now,
        &|_| None,
    ));
    let detail = service::get(&home, &alice(), &RoutineKey::parse(&id).unwrap()).unwrap();
    assert!(matches!(
        &detail.def.triggers[0],
        RoutineTrigger::Schedule(spec) if spec.tz == "America/Denver"
    ));
}

/// A new zone alone moves every schedule into it and keeps each one.
#[test]
fn a_new_zone_alone_keeps_every_schedule() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({ "when": "weekdays 09:00" }), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let key = RoutineKey::parse(&id).unwrap();
    let mut def = service::get(&home, &alice(), &key).unwrap().def;
    def.triggers.push(RoutineTrigger::Schedule(ScheduleSpec {
        cron: "0 17 * * 5".into(),
        tz: "UTC".into(),
    }));
    service::save(
        &home,
        &alice(),
        SaveTarget::Update {
            key: key.clone(),
            base_revision: 1,
        },
        def,
        &Author::agent("t"),
    )
    .unwrap();

    ok(update(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({ "id": id, "baseRevision": 2, "tz": "Europe/London" })),
        now,
        &|_| None,
    ));
    let triggers = service::get(&home, &alice(), &key).unwrap().def.triggers;
    assert_eq!(
        triggers,
        vec![
            RoutineTrigger::Schedule(ScheduleSpec {
                cron: "0 9 * * 1-5".into(),
                tz: "Europe/London".into(),
            }),
            RoutineTrigger::Schedule(ScheduleSpec {
                cron: "0 17 * * 5".into(),
                tz: "Europe/London".into(),
            }),
        ]
    );
}

#[test]
fn manual_result_authoring_and_editor_roundtrip_preserve_schema() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let schema =
        json!({"type":"object","properties":{"count":{"type":"integer"}},"required":["count"]});
    let created = ok(create(
        &home,
        &alice(),
        team(),
        &CallerSession::default(),
        &Author::agent("test"),
        params(
            json!({"name":"Manual result","task":"Collect data","resultSchema":schema,"runsOn":"each_mac","agent":AGENT}),
        ),
        Utc::now(),
    ));
    assert_eq!(created["next_fires"], json!([]));
    let key = RoutineKey::parse(created["id"].as_str().unwrap()).unwrap();
    let initial = service::get_current(&home, &alice(), &key).unwrap();
    assert!(initial.def.triggers.is_empty());
    assert_eq!(initial.def.result_schema, Some(schema.clone()));
    assert_eq!(initial.def.runs_on, "each_mac");
    let caller = crate::commands::routines::Caller {
        home: home.clone(),
        principal: alice(),
    };
    crate::commands::routines::save(
        &caller,
        crate::commands::routines::FormTarget::Update {
            key: key.to_string(),
            base_revision: 1,
        },
        crate::commands::routines::RoutineForm {
            name: Some("Renamed".into()),
            runs_on: Some("one_mac".into()),
            ..Default::default()
        },
        &Author::user("alice"),
        &|_| None,
    )
    .unwrap();
    assert_eq!(
        service::get_current(&home, &alice(), &key)
            .unwrap()
            .def
            .result_schema,
        Some(schema)
    );
    assert_eq!(
        service::get_current(&home, &alice(), &key)
            .unwrap()
            .def
            .runs_on,
        "one_mac"
    );
    ok(update(
        &home,
        &alice(),
        &Author::agent("test"),
        params(json!({"id":key.to_string(),"baseRevision":2,"resultSchema":null})),
        Utc::now(),
        &|_| None,
    ));
    assert!(service::get_current(&home, &alice(), &key)
        .unwrap()
        .def
        .result_schema
        .is_none());
    ok(update(
        &home,
        &alice(),
        &Author::agent("test"),
        params(json!({"id":key.to_string(),"baseRevision":3,"resultSchema":{"type":"array"}})),
        Utc::now(),
        &|_| None,
    ));
    let clear: crate::commands::routines::RoutineForm =
        serde_json::from_value(json!({"resultSchema":null})).unwrap();
    assert_eq!(clear.result_schema, Some(None));
    crate::commands::routines::save(
        &caller,
        crate::commands::routines::FormTarget::Update {
            key: key.to_string(),
            base_revision: 4,
        },
        clear,
        &Author::user("alice"),
        &|_| None,
    )
    .unwrap();
    assert!(service::get_current(&home, &alice(), &key)
        .unwrap()
        .def
        .result_schema
        .is_none());
}

/// An agent creates a recipe each teammate's Mac runs, sees its mode and this
/// Mac's switch in the list and get, turns it on for this Mac, and switches it
/// to a shared job and back, all through the tools' own params.
#[test]
fn an_agent_sets_up_a_recipe_each_mac_runs_and_switches_its_mode() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let created = ok_wire(create_echo(
        &home,
        json!({ "name": "inbox", "runsOn": "each_mac" }),
        now,
    ));
    // Off on every Mac until its user turns it on, so no fires to report.
    assert_eq!(created["enabled"], false);
    assert_eq!(created["next_fires"], json!([]));
    let id = created["id"].as_str().unwrap().to_string();
    let listed = || {
        ok(list(&home, &alice(), Default::default(), now))
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == id.as_str())
            .cloned()
            .unwrap()
    };
    assert_eq!(listed()["runs_on"], "each_mac");
    assert_eq!(listed()["enabled"], false);
    assert_eq!(listed()["armed_here"], true);
    assert!(listed()["next_run_at"].is_null());

    // Turned on for this Mac: a local switch, not an edit.
    ok(set_enabled(
        &home,
        &alice(),
        params(json!({ "id": id, "enabled": true })),
    ));
    assert_eq!(listed()["enabled"], true);
    assert!(listed()["next_run_at"].is_string());
    let got = ok(get(&home, &alice(), params(json!({ "id": id })), now));
    assert_eq!(
        (&got["runs_on"], &got["enabled"], &got["revision"]),
        (&json!("each_mac"), &json!(true), &json!(1))
    );
    assert_eq!(got["routine"]["armed_host"], "");

    // Switched to a shared job: it runs on this Mac, the one that changed it.
    let edit = |base: u32, mode: &str| {
        wire(update(
            &home,
            &alice(),
            &CallerSession::default().author(),
            params(json!({ "id": id, "baseRevision": base, "runsOn": mode })),
            now,
            &|_| None,
        ))
    };
    let shared = ok_wire(edit(1, "one_mac"));
    assert_eq!(
        (&shared["enabled"], &shared["revision"]),
        (&json!(true), &json!(2))
    );
    assert_eq!(shared["next_fires"].as_array().unwrap().len(), 3);
    assert_eq!(listed()["runs_on"], "one_mac");
    assert_eq!(
        ok(get(&home, &alice(), params(json!({ "id": id })), now))["routine"]["armed_host"],
        local_host_id(&home).unwrap().as_str()
    );
    // And back: this Mac ran it, so it stays on here.
    let back = ok_wire(edit(2, "each_mac"));
    assert_eq!(back["enabled"], true);

    // An unknown mode, or a recipe in a personal space, is validation.
    assert_eq!(edit(3, "every_mac")["status"], "validation");
    // A dry run refuses it just as the create does.
    for dry_run in [false, true] {
        let personal = wire(create(
            &home,
            &alice(),
            Placement::Personal(PersonalOwner {
                github_user_id: 1,
                username: "alice".into(),
                display_name: "Alice".into(),
                avatar_url: None,
                org: "acme/api".into(),
            }),
            &CallerSession::default(),
            &CallerSession::default().author(),
            params(json!({
                "name": "mine", "task": "t", "when": "every 5m",
                "runsOn": "each_mac", "dryRun": dry_run, "agent": "acme/api/backend/default",
            })),
            now,
        ));
        assert_eq!(personal["status"], "validation", "{dry_run} {personal}");
        assert!(personal["data"]["message"]
            .as_str()
            .unwrap()
            .contains("team routines"));
    }
    // A shared job is the default and is not written into the file.
    let job = ok_wire(create_echo(&home, json!({ "name": "job" }), now));
    let job_def = ok(get(
        &home,
        &alice(),
        params(json!({ "id": job["id"] })),
        now,
    ));
    assert_eq!(job_def["runs_on"], "one_mac");
    assert!(job_def["routine"].get("runs_on").is_none());
}

/// An agent's whole loop with a named agent, over MCP: create a routine that
/// names `triage`, read it back, run it, and see the run's agent revision;
/// then the agent cannot be hard-deleted while the routine names it, and
/// archiving it reports the routine.
#[tokio::test]
async fn a_routine_names_an_agent_runs_it_and_keeps_it_from_being_deleted() {
    use crate::mcp::agent_tools::{self, AgentCaller};
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let agent_key = save_team_agent(&home, &alice(), test_agent("triage", AgentCli::Claude));

    let id = ok_wire(create_echo(&home, json!({ "agent": "triage" }), now))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let got = ok(get(&home, &alice(), params(json!({ "id": id })), now));
    assert_eq!(got["routine"]["agent"], "triage");
    let listed = ok(list(&home, &alice(), Default::default(), now));
    assert_eq!(listed[0]["agent"], "triage");

    ok(run_now(&home, &alice(), params(json!({ "id": id }))));
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let runs = runs_of(&home, &id);
    assert_eq!(runs["runs"][0]["agent"], "acme/api/backend/triage@1");
    let session_id = launcher.last().session_id;
    let context = ok(context(&home, &session_id, None));
    assert_eq!(context["agent"], "acme/api/backend/triage@1");

    let caller = AgentCaller::new(&alice(), None);
    let in_use = serde_json::to_value(
        agent_tools::delete(
            &home,
            &alice(),
            &caller,
            &crate::routines::agents::RoutineReferrers,
            params(json!({ "id": agent_key.to_string(), "hard": true })),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(in_use["ok"], false, "{in_use}");
    assert_eq!(in_use["error"]["code"], "in_use");
    assert_eq!(in_use["error"]["referrers"], json!([id]));
    let archived = serde_json::to_value(
        agent_tools::delete(
            &home,
            &alice(),
            &caller,
            &crate::routines::agents::RoutineReferrers,
            params(json!({ "id": agent_key.to_string() })),
        )
        .unwrap(),
    )
    .unwrap();
    // Archiving it would archive its routine: refused until confirmed, listing it.
    assert_eq!(archived["error"]["code"], "needs_confirm", "{archived}");
    assert_eq!(archived["error"]["referrers"], json!([id]));
    let archived = serde_json::to_value(
        agent_tools::delete(
            &home,
            &alice(),
            &caller,
            &crate::routines::agents::RoutineReferrers,
            params(json!({ "id": agent_key.to_string(), "archiveRoutines": true })),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        archived["data"]["archived_routines"][0]["id"],
        json!(id),
        "{archived}"
    );
    assert_eq!(
        archived["data"]["used_by"],
        json!([]),
        "its routine went with it"
    );
    launcher.exit(&session_id).await;
}

/// A task whose leading `/command` the agent does not start with saves, with
/// the warning; once the agent starts with it, the warning is gone.
#[test]
fn a_task_starting_with_a_command_the_agent_lacks_saves_with_a_warning() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let agent_key = save_team_agent(&home, &alice(), test_agent("triage", AgentCli::Claude));
    const WARNING: &str =
        "this agent doesn't start with /triage-slack — add it to the agent or the run won't find it";

    let created = ok_wire(create_echo(
        &home,
        json!({ "agent": "triage", "task": "/triage-slack today" }),
        now,
    ));
    assert_eq!(
        created["warnings"],
        json!([{ "code": "command_not_in_manifest", "message": WARNING }])
    );
    assert_eq!(created["revision"], 1, "the save went through");
    let dry = ok_wire(create_echo(
        &home,
        json!({ "name": "dry", "agent": "triage", "task": "/triage-slack", "dryRun": true }),
        now,
    ));
    assert_eq!(dry["warnings"][0]["message"], WARNING);

    // The agent starts with the command: the warning clears.
    let content = crate::context_repo::path_to_team_content_dir(
        &path_to_org_root(&home, &alice(), ORG).unwrap(),
        TEAM,
    );
    std::fs::create_dir_all(content.join(".claude/commands")).unwrap();
    std::fs::write(
        content.join(".claude/commands/triage-slack.md"),
        "Triage.\n",
    )
    .unwrap();
    let current = crate::agents::service::get(&home, &alice(), &agent_key).unwrap();
    crate::agents::service::save(
        &home,
        &alice(),
        crate::agents::SaveTarget::Update {
            key: agent_key.clone(),
            base_revision: current.agent.revision,
        },
        crate::agents::model::Agent {
            startup: vec![".claude/commands/triage-slack.md".into()],
            ..current.agent
        },
        &Author::user("alice"),
    )
    .unwrap();
    let updated = ok(update(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({
            "id": created["id"], "baseRevision": 1, "task": "/triage-slack tomorrow"
        })),
        now,
        &|_| None,
    ));
    assert_eq!(updated["warnings"], json!([]));
}

/// A routine whose agent went away stays editable: an update that resends
/// the agent it already names (as the tab does) saves and warns, while
/// naming a new agent nobody can see is still refused.
#[test]
fn a_routine_whose_agent_went_away_stays_editable() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let agent_key = save_team_agent(&home, &alice(), test_agent("triage", AgentCli::Claude));
    let created = ok_wire(create_echo(&home, json!({ "agent": "triage" }), now));
    // A teammate's delete arrives with a pull: the agent's own files go, its
    // `routines/` stays.
    let agent_dir = crate::agents::roots::team_agents_dir(&home, &alice(), ORG, TEAM)
        .unwrap()
        .unwrap()
        .join(&agent_key.id);
    std::fs::remove_file(agent_dir.join("agent.yaml")).unwrap();
    std::fs::remove_dir_all(agent_dir.join("revisions")).unwrap();

    let renamed = ok(update(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({
            "id": created["id"], "baseRevision": 1, "agent": "triage", "name": "Renamed"
        })),
        now,
        &|_| None,
    ));
    assert_eq!(renamed["revision"], 2);
    assert_eq!(renamed["agent"], "triage");
    assert_eq!(
        renamed["warnings"][0]["code"], "agent_unavailable",
        "{renamed}"
    );

    let refused = wire(update(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({ "id": created["id"], "baseRevision": 2, "agent": "nobody" })),
        now,
        &|_| None,
    ));
    assert_eq!(refused["status"], "validation", "{refused}");
    assert!(refused["data"]["message"]
        .as_str()
        .unwrap()
        .contains("agent_list"));
}

/// A personal routine (and agent) filed over MCP for one org lives in that
/// org's scratch space only: it is keyed by that org, describe_setup counts it
/// there and nowhere else, and the Routines tab shows it only in its own org.
#[test]
fn a_scratch_routine_filed_for_one_org_never_appears_in_another() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    save_personal_agent(&home, &alice(), test_agent("mine", AgentCli::Claude));
    let created = ok(create(
        &home,
        &alice(),
        Placement::Personal(PersonalOwner {
            github_user_id: 1,
            username: "alice".into(),
            display_name: "Alice".into(),
            avatar_url: None,
            org: ORG.into(),
        }),
        &CallerSession::default(),
        &CallerSession::default().author(),
        params(json!({ "name": "Digest", "task": "t", "when": "daily 09:00", "agent": "mine" })),
        now,
    ));
    let key = RoutineKey::parse(created["id"].as_str().unwrap()).unwrap();
    assert_eq!(key.org, ORG);

    let other = "acme/other";
    let scratch =
        crate::mcp::setup_tools::describe_scratch(&home, &alice(), &[ORG.into(), other.into()]);
    let counts = |org: &str| {
        scratch
            .orgs
            .iter()
            .find(|o| o.org == org)
            .map(|o| (o.exists, o.counts.routines, o.counts.agents))
            .unwrap()
    };
    assert_eq!(counts(ORG), (true, 1, 1));
    assert_eq!(counts(other), (false, 0, 0));

    let in_view = |active: &str| {
        crate::context_repo::scratch_space::set_active_org(&home, Some(active));
        crate::routines::discover::discover_routines_in_view(&home, &alice())
            .unwrap()
            .into_iter()
            .any(|r| r.key == key)
    };
    assert!(in_view(ORG));
    assert!(!in_view(other), "another org's Routines tab never lists it");
}

// `routine_done` finishes only the named run, and only
// for the session running it. The session's other run is untouched (a woken
// session can hold one from an earlier wake), and nothing session-wide
// happens: unlike `signal_done` there is no agent-done, no activity change.
#[test]
fn routine_done_finishes_only_that_run_and_not_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let store = RoutineStateStore::open(home).unwrap();
    let conn = rusqlite::Connection::open(crate::coordinator::default_store_path(home)).unwrap();
    for (run, routine) in [("run-a", "triage"), ("run-b", "digest")] {
        conn.execute(
            "INSERT INTO routine_runs (run_id, routine_key, revision, source, phase, session_id, created_at, started_at)
             VALUES (?1, ?2, 1, 'schedule', 'running', 's-barb', 0, 0)",
            rusqlite::params![run, format!("acme/core/barb/{routine}")],
        )
        .unwrap();
    }
    let params = |run: &str, outcome: &str| -> RoutineDoneParams {
        serde_json::from_value(json!({
            "sessionId": "s-barb", "runId": run, "outcome": outcome, "summary": "  3 issues triaged  "
        }))
        .unwrap()
    };
    // Another session cannot finish Barb's run.
    let refused = done(&store, "s-other", params("run-a", "success"), 5).unwrap();
    assert_eq!(
        serde_json::to_value(&refused).unwrap(),
        json!({"status": "validation", "data": {"message": "that run is not this session's"}})
    );
    assert_eq!(store.run("run-a").unwrap().unwrap().phase, "running");
    // A bad outcome changes nothing.
    let bad = done(&store, "s-barb", params("run-a", "done"), 5).unwrap();
    assert!(matches!(bad, ToolEnvelope::Validation { .. }));
    let reply = done(&store, "s-barb", params("run-a", "success"), 5).unwrap();
    assert_eq!(
        serde_json::to_value(&reply).unwrap(),
        json!({"status": "ok", "data": {"run_id": "run-a", "routine_id": "acme/core/barb/triage", "phase": "succeeded"}})
    );
    let a = store.run("run-a").unwrap().unwrap();
    assert_eq!(a.result_text.as_deref(), Some("3 issues triaged"));
    assert_eq!(
        store.run("run-b").unwrap().unwrap().phase,
        "running",
        "the session's other run is untouched"
    );
    // Once is enough: a second call reports where the run stands.
    let again = done(&store, "s-barb", params("run-a", "failed"), 6).unwrap();
    assert_eq!(
        serde_json::to_value(&again).unwrap(),
        json!({"status": "validation", "data": {"message": "that run is not running (it is succeeded)"}})
    );
    let missing = done(&store, "s-barb", params("run-z", "failed"), 6).unwrap();
    assert!(
        matches!(missing, ToolEnvelope::Validation { message } if message == "no such routine run")
    );
}

// `routine_done` takes exactly its four fields.
#[test]
fn routine_done_params_are_strict() {
    assert!(serde_json::from_value::<RoutineDoneParams>(
        json!({"sessionId": "s", "runId": "r", "outcome": "success", "extra": 1})
    )
    .is_err());
    let p: RoutineDoneParams =
        serde_json::from_value(json!({"session_id": "s", "run_id": "r", "outcome": "failed"}))
            .unwrap();
    assert_eq!((p.session_id.as_str(), p.summary), ("s", None));
}

// Adding wake limits to a wake routine armed on a machine
// whose terminal server can't enforce them is saved with a warning that it
// fires nowhere there; `routine_arm_on` asks the target with the routine's
// limits in mind.
#[test]
fn limits_on_a_routine_armed_where_they_cannot_be_enforced_warn() {
    let _temp = TempHome::new();
    let home = _temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(
        &home,
        json!({"mode": "wake", "visible": true, "when": "daily 09:00"}),
        now,
    )["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let asked = std::sync::Mutex::new(Vec::new());
    let armed = wire(arm_on(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({ "id": id, "machine": "m-old" })),
        &|machine, limited| {
            asked.lock().unwrap().push((machine.to_string(), limited));
            Ok(None)
        },
    ));
    assert_eq!(armed["status"], "ok", "{armed}");
    let revision = armed["data"]["revision"].as_u64().unwrap();
    let note = |machine: &str| {
        (machine == "m-old").then(|| "m-old's terminal server is too old".to_string())
    };
    let limited = wire(update(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({ "id": id, "base_revision": revision, "minGap": "2h" })),
        now,
        &note,
    ));
    assert_eq!(limited["status"], "ok", "{limited}");
    let warnings = limited["data"]["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w["code"] == "wake_limits_unenforced"
                && w["message"] == "m-old's terminal server is too old"),
        "{warnings:?}"
    );
    // Armed again with its limits: the target is asked knowing it is limited.
    wire(arm_on(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({ "id": id, "machine": "m-old" })),
        &|machine, limited| {
            asked.lock().unwrap().push((machine.to_string(), limited));
            Ok(None)
        },
    ));
    assert_eq!(
        asked.lock().unwrap().clone(),
        [("m-old".to_string(), false), ("m-old".to_string(), true)]
    );
    // Without limits, nothing to warn about.
    let current = service::get_current(&home, &alice(), &RoutineKey::parse(&id).unwrap()).unwrap();
    let cleared = wire(update(
        &home,
        &alice(),
        &CallerSession::default().author(),
        params(json!({ "id": id, "base_revision": current.def.revision, "minGap": null })),
        now,
        &note,
    ));
    assert!(!cleared["data"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["code"] == "wake_limits_unenforced"));
}

#[test]
fn metadata_conditions_roundtrip_replace_remove_and_refuse_each_mac() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-20T12:00:00Z");
    let target = json!({"selection":{"where":{"kinds":["story"],"pathPrefix":"stories/"}},"target":{"key":"status","op":"eq","value":"in-progress"}});
    let created = ok_wire(create_echo(
        &home,
        json!({"when":"","metadataTriggers":[target.clone()]}),
        now,
    ));
    assert!(created["next_fires"].as_array().unwrap().is_empty());
    let id = created["id"].as_str().unwrap();
    let read = service::get(&home, &alice(), &RoutineKey::parse(id).unwrap()).unwrap();
    assert_eq!(read.def.triggers.len(), 1);
    let mut exact = target.clone();
    exact["previous"] = json!({"key":"status","op":"eq","value":"planning"});
    let updated = ok(update(
        &home,
        &alice(),
        &Author::user("alice"),
        params(
            json!({"id":id,"baseRevision":created["revision"],"metadataTriggers":[exact.clone()]}),
        ),
        now,
        &|_| None,
    ));
    let read = service::get(&home, &alice(), &RoutineKey::parse(id).unwrap()).unwrap();
    assert!(
        matches!(&read.def.triggers[0],RoutineTrigger::Event(e) if e.metadata.as_ref().unwrap().previous.is_some())
    );
    let refused = wire(update(
        &home,
        &alice(),
        &Author::user("alice"),
        params(json!({"id":id,"baseRevision":updated["revision"],"runsOn":"each_mac"})),
        now,
        &|_| None,
    ));
    assert_eq!(refused["status"], "validation");
    let removed = ok(update(
        &home,
        &alice(),
        &Author::user("alice"),
        params(
            json!({"id":id,"baseRevision":updated["revision"],"when":"5m","metadataTriggers":[]}),
        ),
        now,
        &|_| None,
    ));
    let read = service::get(&home, &alice(), &RoutineKey::parse(id).unwrap()).unwrap();
    assert!(matches!(
        &read.def.triggers[..],
        [RoutineTrigger::Schedule(_)]
    ));
    assert!(removed["revision"].as_u64().unwrap() > updated["revision"].as_u64().unwrap());
}

/// The cookbook and command-chain pages render these very inputs, so their examples
/// exercise the same create dry-run boundary that an MCP caller uses.
#[test]
fn cookbook_examples_use_real_create_dry_run_and_event_yaml_validation() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let examples: serde_json::Value = serde_json::from_str(include_str!(
        "../../../codesense-docs/getting-started/content/examples/routines/cookbook.json"
    ))
    .unwrap();
    let now = at("2026-10-22T09:01:00Z");
    for example in examples["examples"].as_array().unwrap() {
        let args = example["mcp"].clone();
        let before = files_under(&home);
        let preview = ok(create(
            &home,
            &alice(),
            team(),
            &CallerSession::default(),
            &Author::agent("cookbook"),
            params(args.clone()),
            now,
        ));
        assert_eq!(preview["dry_run"], true, "{example}");
        // State/coordinator reads can initialize their databases; no routine definition is written.
        let added = files_under(&home)
            .difference(&before)
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            !added
                .iter()
                .any(|p| p.extension().is_some_and(|e| e == "yml")),
            "{added:?}"
        );
        let pages = [
            include_str!("../../../codesense-docs/getting-started/content/arc-g-routines/trigger-cookbook.html"),
            include_str!("../../../codesense-docs/getting-started/content/arc-g-routines/command-routines.html"),
        ];
        let published_yaml = pages
            .iter()
            .flat_map(|page| {
                page.split("<code class=\"language-yaml\">")
                    .skip(1)
                    .map(|block| {
                        let text = block.split("</code>").next().unwrap();
                        text.replace("&lt;", "<")
                            .replace("&gt;", ">")
                            .replace("&amp;", "&")
                    })
            })
            .find(|text| {
                serde_yaml::from_str::<serde_json::Value>(text)
                    .is_ok_and(|wire| wire == example["yaml"])
            })
            .expect("the copyable published YAML is the exact tested example");
        let def: RoutineDef = serde_yaml::from_str(&published_yaml).unwrap();
        def.validate().unwrap();
        let key = RoutineKey::scoped(&alice(), ORG, TEAM, &def.id);
        def.validate_agents_for(&key).unwrap();
        assert_eq!(
            preview["triggers"],
            serde_json::to_value(&def.triggers).unwrap(),
            "{example}"
        );
        // Commit disposable drafts in dependency order so the downstream dry run
        // checks a real predecessor in the same space, with real agent fixtures.
        let mut save_args = args;
        save_args["dryRun"] = json!(false);
        ok(create(
            &home,
            &alice(),
            team(),
            &CallerSession::default(),
            &Author::agent("cookbook"),
            params(save_args),
            now,
        ));
    }
}

#[test]
fn events_round_trip_preview_patch_and_validate_through_the_shared_model() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-22T09:01:00Z");
    let events = json!([
        {"kind":"inbox_idle"},
        {"kind":"agent_done", "from_agent":"not-installed"},
        {"kind":"role_pending", "role":" role: reviewer "},
        {"kind":"role_pending"}
    ]);
    let handwritten: RoutineDef = serde_yaml::from_str(
        r#"
id: events
name: Events
owner: "1"
agent: default
task: Inspect events
where: {kind: team_workspace}
policy: {max_runtime: 5m}
revision: 1
triggers:
  - event: {kind: inbox_idle}
  - event: {kind: agent_done, from_agent: not-installed}
  - event: {kind: role_pending, role: reviewer}
  - event: {kind: role_pending}
"#,
    )
    .unwrap();
    let expected = handwritten.triggers;
    local_host_id(&home).unwrap();
    let before = files_under(&home);
    let preview = ok_wire(create_echo(
        &home,
        json!({"events":events, "dryRun":true}),
        now,
    ));
    assert_eq!(
        preview["triggers"].as_array().unwrap()[1..],
        serde_json::to_value(&expected).unwrap().as_array().unwrap()[..]
    );
    assert_eq!(files_under(&home), before, "event preview writes nothing");
    let saved = ok_wire(create_echo(
        &home,
        json!({"events":events, "runsOn":"each_mac"}),
        now,
    ));
    let id = saved["id"].as_str().unwrap();
    let key = RoutineKey::parse(id).unwrap();
    let original = service::get(&home, &alice(), &key).unwrap().def;
    assert_eq!(&original.triggers[1..], expected.as_slice());
    let got = ok(get(&home, &alice(), params(json!({"id":id})), now));
    assert_eq!(got["routine"]["triggers"][3]["event"]["role"], "reviewer");
    let round_trip: RoutineDef =
        serde_yaml::from_str(&serde_yaml::to_string(&original).unwrap()).unwrap();
    assert_eq!(round_trip, original);
    let mut revision = saved["revision"].as_u64().unwrap();
    for patch in [json!({}), json!({"events":null})] {
        let mut args = patch;
        args["id"] = json!(id);
        args["baseRevision"] = json!(revision);
        let updated = ok(update(
            &home,
            &alice(),
            &Author::agent("test"),
            params(args),
            now,
            &|_| None,
        ));
        revision = updated["revision"].as_u64().unwrap();
        assert_eq!(
            service::get(&home, &alice(), &key).unwrap().def.triggers,
            original.triggers
        );
    }
    // The matrix uses both create and update: every refusal must identify the
    // supplied event index and leave the persisted revision/definition untouched.
    for (event, field, hint) in [
        (json!({"kind":"future_event"}), "kind", "inbox_idle"),
        (
            json!({"kind":"metadata_change"}),
            "kind",
            "metadataTriggers",
        ),
        (
            json!({"kind":"inbox_idle","from_agent":"reviewer"}),
            "from_agent",
            "agent_done",
        ),
        (
            json!({"kind":"role_pending","from_agent":"reviewer"}),
            "from_agent",
            "agent_done",
        ),
        (
            json!({"kind":"agent_done","from_agent":""}),
            "from_agent",
            "agent",
        ),
        (
            json!({"kind":"agent_done","from_agent":"codex:reviewer"}),
            "from_agent",
            "agent",
        ),
        (
            json!({"kind":"role_pending","role":"  "}),
            "role",
            "omit role",
        ),
        (
            json!({"kind":"role_pending","role":""}),
            "role",
            "omit role",
        ),
        (
            json!({"kind":"role_pending","role":"role:  "}),
            "role",
            "omit role",
        ),
        (
            json!({"kind":"agent_done","role":"reviewer"}),
            "role",
            "role_pending",
        ),
        (
            json!({"kind":"inbox_idle","role":"reviewer"}),
            "role",
            "role_pending",
        ),
    ] {
        let input = json!([{"kind":"inbox_idle"}, event]);
        let persisted = service::get(&home, &alice(), &key).unwrap().def;
        let before = files_under(&home);
        let replies = [
            create_echo(&home, json!({"id":"refused", "events":input}), now),
            wire(update(
                &home,
                &alice(),
                &Author::agent("test"),
                params(json!({"id":id,"baseRevision":revision,"events":input})),
                now,
                &|_| None,
            )),
        ];
        for reply in replies {
            assert_eq!(reply["status"], "validation", "{reply}");
            let message = reply["data"]["message"].as_str().unwrap();
            assert!(message.contains(&format!("events[1].{field}")), "{message}");
            assert!(message.contains(hint), "{message}");
        }
        assert_eq!(files_under(&home), before);
        assert_eq!(service::get(&home, &alice(), &key).unwrap().def, persisted);
    }
    // Replacing events must preserve a schedule and an independent metadata trigger.
    let condition = json!({"selection":{},"target":{"key":"status","op":"eq","value":"review"}});
    let mixed = ok_wire(create_echo(
        &home,
        json!({"id":"mixed", "events":events, "metadataTriggers":[condition]}),
        now,
    ));
    let mixed_id = mixed["id"].as_str().unwrap();
    let replacement = ok(update(
        &home,
        &alice(),
        &Author::agent("test"),
        params(
            json!({"id":mixed_id,"baseRevision":mixed["revision"],"events":[{"kind":"role_pending","role":" role: builder "}]}),
        ),
        now,
        &|_| None,
    ));
    assert_eq!(replacement["triggers"].as_array().unwrap().len(), 3);
    assert_eq!(replacement["triggers"][0], mixed["triggers"][0]);
    assert_eq!(replacement["triggers"][1], mixed["triggers"][1]);
    assert_eq!(
        replacement["triggers"][2],
        json!({"event":{"kind":"role_pending","role":"builder"}})
    );
    let cleared = ok(update(
        &home,
        &alice(),
        &Author::agent("test"),
        params(json!({"id":mixed_id,"baseRevision":replacement["revision"],"events":[]})),
        now,
        &|_| None,
    ));
    assert_eq!(
        cleared["triggers"],
        json!([mixed["triggers"][0], mixed["triggers"][1]])
    );
}

#[tokio::test]
async fn cancelling_one_running_run_stops_its_agent_and_releases_held_work() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({"overlap":"queue"}), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let first = ok(run_now(&home, &alice(), params(json!({"id":id}))));
    let first_id = first["run_id"].as_str().unwrap();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let session = launcher.last().session_id;
    assert!(launcher.is_running(&session));
    let held = ok(run_now(&home, &alice(), params(json!({"id":id}))));
    let held_id = held["run_id"].as_str().unwrap();
    assert_eq!(held["reason"], "held");

    let cancelled = ok(cancel(
        &home,
        &alice(),
        params(json!({"runId":first_id})),
        launcher.as_ref(),
    ));
    assert_eq!(cancelled["phase"], "cancelled");
    assert_eq!(cancelled["run_id"], first_id);
    assert_eq!(cancelled["routine_id"], id);
    assert!(cancelled["ended_at"].as_str().unwrap().ends_with('Z'));
    assert!(!launcher.is_running(&session));
    assert_eq!(
        cancelled,
        ok(run(&home, &alice(), params(json!({"runId":first_id}))))
    );
    let store = RoutineStateStore::open(&home).unwrap();
    let released = store.run(held_id).unwrap().unwrap();
    assert_eq!(released.phase, "queued");
    assert_eq!(released.reason, None);
    assert_eq!(launcher.calls(), 1, "held work has not launched yet");

    let sessions = crate::routines::test_support::FakeSessions::default();
    let queued = ok(cancel(
        &home,
        &alice(),
        params(json!({"run_id":held_id})),
        &sessions,
    ));
    assert_eq!(queued["phase"], "cancelled");
    assert_eq!(*sessions.killed.lock().unwrap(), vec![held_id.to_string()]);
    let before_repeat = store.run(held_id).unwrap().unwrap();
    let again = wire(cancel(
        &home,
        &alice(),
        params(json!({"runId":held_id})),
        &sessions,
    ));
    assert_eq!(again["status"], "validation");
    assert!(again["data"]["message"]
        .as_str()
        .unwrap()
        .contains("already finished (cancelled)"));
    assert_eq!(sessions.killed.lock().unwrap().len(), 1);
    assert_eq!(store.run(held_id).unwrap().unwrap(), before_repeat);
}

#[tokio::test]
async fn cancelling_a_successful_run_preserves_its_record_without_another_kill() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let id = create_echo(&home, json!({}), now)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let fired = ok(run_now(&home, &alice(), params(json!({"id":id}))));
    let run_id = fired["run_id"].as_str().unwrap();
    let launcher = Arc::new(StubLauncher::new(stub_agent(temp.root())));
    assert_eq!(drain_claim_loop(&home, launcher.clone()).await, 1);
    let session = launcher.last().session_id;
    launcher.signal_done(&session, "success", "done").await;
    launcher.exit(&session).await;
    let before = RoutineStateStore::open(&home)
        .unwrap()
        .run(run_id)
        .unwrap()
        .unwrap();
    let sessions = crate::routines::test_support::FakeSessions::default();
    let result = wire(cancel(
        &home,
        &alice(),
        params(json!({"runId":run_id})),
        &sessions,
    ));
    assert_eq!(result["status"], "validation");
    assert!(result["data"]["message"]
        .as_str()
        .unwrap()
        .contains("already finished (succeeded)"));
    assert_eq!(
        RoutineStateStore::open(&home)
            .unwrap()
            .run(run_id)
            .unwrap()
            .unwrap(),
        before
    );
    assert!(sessions.killed.lock().unwrap().is_empty());
}

#[test]
fn cancelling_unknown_and_hidden_personal_runs_discloses_no_private_state() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let now = at("2026-10-05T09:01:00Z");
    let personal = ok(create(
        &home,
        &alice(),
        Placement::Personal(PersonalOwner {
            github_user_id: 1,
            username: "alice".into(),
            display_name: "Alice".into(),
            avatar_url: None,
            org: ORG.into(),
        }),
        &CallerSession::default(),
        &CallerSession::default().author(),
        params(
            json!({"name":"private", "task":"work", "when":"every 5m", "agent":format!("{ORG}/{TEAM}/{AGENT}")}),
        ),
        now,
    ));
    let fired = ok(run_now(
        &home,
        &alice(),
        params(json!({"id":personal["id"]})),
    ));
    let run_id = fired["run_id"].as_str().unwrap();
    let sessions = crate::routines::test_support::FakeSessions::default();
    let bob = Principal::from(2u64);
    write_org(&home, 2);
    let hidden = wire(cancel(
        &home,
        &bob,
        params(json!({"runId":run_id})),
        &sessions,
    ));
    assert_eq!(
        hidden,
        json!({"status":"validation", "data":{"message":format!("routine run {run_id} not found")}})
    );
    let unknown = wire(cancel(
        &home,
        &bob,
        params(json!({"runId":"missing"})),
        &sessions,
    ));
    assert_eq!(
        unknown,
        json!({"status":"validation", "data":{"message":"routine run missing not found"}})
    );
    assert_eq!(
        RoutineStateStore::open(&home)
            .unwrap()
            .run(run_id)
            .unwrap()
            .unwrap()
            .phase,
        "queued"
    );
    assert!(sessions.killed.lock().unwrap().is_empty());
}

#[test]
fn child_cancellation_hides_ancestor_runs_before_stopping_and_keeps_own_runs() {
    use crate::context_repo::caller_scope::tests::Fixture;
    let f = Fixture::new();
    let me = Principal::from(1u64);
    let store = crate::routines::state::RoutineStateStore::open(&f.home).unwrap();
    let db = rusqlite::Connection::open(store.path()).unwrap();
    for team in ["parent", "child", "sibling"] {
        let key = f.routine(team, "cancel-job");
        db.execute(
            "INSERT INTO routine_runs (run_id,routine_key,revision,source,phase,created_at) VALUES (?1,?2,1,'manual','queued',0)",
            rusqlite::params![format!("{team}-run"), key],
        ).unwrap();
    }
    let sessions = crate::routines::test_support::FakeSessions::default();
    let call = |id: &str| {
        wire(cancel_for(
            &f.home,
            &me,
            RoutineCancelParams {
                run_id: id.into(),
                session_id: None,
            },
            &sessions,
            &f.scope,
        ))
    };
    let missing = call("missing-run");
    assert_eq!(call("parent-run"), missing);
    let parent_key = RoutineKey::parse("acme/ctx/parent/cancel-job").unwrap();
    std::fs::remove_file(
        crate::routines::service::routine_dir_of(&f.home, &me, &parent_key)
            .unwrap()
            .join("routine.yml"),
    )
    .unwrap();
    assert_eq!(call("parent-run"), missing);
    assert_eq!(store.run("parent-run").unwrap().unwrap().phase, "queued");
    assert!(sessions.killed.lock().unwrap().is_empty());
    assert!(sessions.killed_on.lock().unwrap().is_empty());
    for team in ["child", "sibling"] {
        let cancelled = call(&format!("{team}-run"));
        assert_eq!(cancelled["status"], "ok");
        assert_eq!(cancelled["data"]["phase"], "cancelled");
    }
    assert!(f
        .scope
        .read_admitted(&f.path("parent", "content/shared.md"))
        .is_ok());
}
