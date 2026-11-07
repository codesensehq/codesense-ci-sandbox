// SPDX-License-Identifier: MIT
use super::*;
use crate::coordinator::OrchestrationStore;
use crate::{commands::weft_ledger, live_hub::projection::TopicRegistry};

fn binding(home: &Path) -> Binding {
    Binding {
        principal: "42".into(),
        weft: home.join("weft.sqlite3"),
        jsonl: home.join("run-store.jsonl"),
        durable: home.join("coord.sqlite3"),
    }
}
fn key_for(topic: &str, run: &str) -> String {
    format!(
        "runReview:{}",
        json!([
            "42",
            "acme/ctx:team",
            if topic == "runEvidence" { "SW-A" } else { "" },
            run
        ])
    )
}
fn project(owner: &Shared, registry: &mut TopicRegistry) -> Vec<Value> {
    owner
        .lock()
        .unwrap()
        .project(registry)
        .unwrap()
        .into_iter()
        .map(|delta| serde_json::to_value(delta).unwrap())
        .collect()
}
fn outcome(run: &str) -> run_store::RunEvent {
    run_store::RunEvent::Outcome(Box::new(serde_json::from_value(json!({"story_id":"SW-A","run_id":run,"status":"success","summary":"Parser complete","ended_at":"2026-10-21T20:00:00Z","source":"signal"})).unwrap()))
}
fn report(run: &str, summary: &str) -> run_store::RunEvent {
    run_store::RunEvent::StepResult(
        serde_json::from_value(
            json!({"story_id":"SW-A","run_id":run,"node_id":"review","summary":summary}),
        )
        .unwrap(),
    )
}

#[test]
fn real_writers_feed_mounted_views_and_hidden_views_catch_up_without_reading_again() {
    let home = tempfile::tempdir().unwrap();
    let binding = binding(home.path());
    let owner: Shared = Default::default();
    let wake = Arc::new(tokio::sync::Notify::new());
    observe(&owner, &wake, &binding);
    let mut hub = TopicRegistry::default();
    let mut frames = Vec::new();
    for run in ["run-1", "run-2"] {
        for topic in TOPICS {
            owner
                .lock()
                .unwrap()
                .prepare(
                    topic,
                    &key_for(topic, run),
                    binding.clone(),
                    "SW-A",
                    run,
                    &format!("{topic}:{run}"),
                )
                .unwrap();
        }
    }
    let other_binding = Binding {
        weft: home.path().join("other-team.sqlite3"),
        ..binding.clone()
    };
    observe(&owner, &wake, &other_binding);
    let other_key = key_for("runEndorsements", "run-1").replace("acme/ctx:team", "acme/ctx:other");
    owner
        .lock()
        .unwrap()
        .prepare(
            "runEndorsements",
            &other_key,
            other_binding,
            "",
            "run-1",
            "other-team",
        )
        .unwrap();
    let reads = owner.lock().unwrap().reads;
    owner
        .lock()
        .unwrap()
        .prepare(
            "runEndorsements",
            &key_for("runEndorsements", "run-1"),
            binding.clone(),
            "",
            "run-1",
            "header-and-panel",
        )
        .unwrap();
    assert_eq!(
        owner.lock().unwrap().reads,
        reads,
        "another view of the same run shares the seed"
    );
    project(&owner, &mut hub);
    for topic in TOPICS {
        frames.push(
            serde_json::to_value(hub.snapshot(topic, &key_for(topic, "run-1")).unwrap()).unwrap(),
        );
    }
    let store = crate::weft::WeftStore::open(&binding.weft).unwrap();
    let assertion = weft_ledger::post_assertion(
        &store,
        "worker",
        Some("run-1"),
        "Ready to review",
        "review",
        None,
    )
    .unwrap();
    assert_eq!(assertion.run_id.as_deref(), Some("run-1"));
    assert!(!assertion.posted_at.is_empty());
    let mut posted = project(&owner, &mut hub);
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0]["upserts"][0]["assertion"]["id"], assertion.id);
    // Normalize the database clock only in the exported deterministic transport fixture.
    posted[0]["upserts"][0]["assertion"]["postedAt"] = json!("2026-10-21T20:00:00Z");
    frames.extend(posted);
    assert_eq!(hub.snapshot("runEndorsements", &other_key).unwrap().rev, 0);
    run_store::append_to(&binding.jsonl, report("run-1", "Parser complete"));
    run_store::append_to(&binding.jsonl, outcome("run-1"));
    frames.extend(project(&owner, &mut hub));
    assert_eq!(
        hub.snapshot("runEvidence", &key_for("runEvidence", "run-1"))
            .unwrap()
            .snapshot
            .len(),
        2
    );
    for topic in TOPICS {
        assert_eq!(
            hub.snapshot(topic, &key_for(topic, "run-2")).unwrap().rev,
            0
        );
    }
    let reads = owner.lock().unwrap().reads;
    let folds = owner.lock().unwrap().folds;
    owner.lock().unwrap().unsubscribe("runEvidence:run-1");
    run_store::append_to(&binding.jsonl, report("run-1", "Hidden update"));
    assert!(project(&owner, &mut hub).is_empty());
    assert_eq!(owner.lock().unwrap().folds, folds);
    assert_eq!(owner.lock().unwrap().reads, reads);
    owner
        .lock()
        .unwrap()
        .prepare(
            "runEvidence",
            &key_for("runEvidence", "run-1"),
            binding.clone(),
            "SW-A",
            "run-1",
            "runEvidence:run-1",
        )
        .unwrap();
    let hidden = project(&owner, &mut hub);
    assert_eq!(hidden[0]["upserts"].as_array().unwrap().len(), 1);
    frames.extend(hidden);
    assert_eq!(owner.lock().unwrap().reads, reads);
    weft_ledger::settle_assertion_row(&store, assertion.id, "ratified").unwrap();
    frames.extend(project(&owner, &mut hub));
    assert!(hub
        .snapshot("runEndorsements", &key_for("runEndorsements", "run-1"))
        .unwrap()
        .snapshot
        .is_empty());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/run-review.json");
    if std::env::var_os("UPDATE_RUN_REVIEW_FIXTURE").is_some() {
        std::fs::write(
            fixture,
            serde_json::to_string_pretty(&frames).unwrap() + "\n",
        )
        .unwrap();
    } else {
        assert_eq!(
            serde_json::from_str::<Value>(&std::fs::read_to_string(fixture).unwrap()).unwrap(),
            json!(frames)
        );
    }
}

#[test]
fn durable_appends_and_revision_gaps_use_the_real_hub() {
    let home = tempfile::tempdir().unwrap();
    let binding = binding(home.path());
    let owner: Shared = Default::default();
    let wake = Arc::new(tokio::sync::Notify::new());
    observe(&owner, &wake, &binding);
    owner
        .lock()
        .unwrap()
        .prepare(
            "runEvidence",
            &key_for("runEvidence", "run-1"),
            binding.clone(),
            "SW-A",
            "run-1",
            "view",
        )
        .unwrap();
    let mut hub = TopicRegistry::default();
    project(&owner, &mut hub);
    let store = SqliteOrchestrationStore::open(&binding.durable).unwrap();
    let stream = durable::durable_run_key("run-1", "SW-A");
    store.append(RunEvent { run_id: stream.clone(), kind: "run.accepted".into(), payload: json!({"provenance":{"storyId":"SW-A","trigger":{"kind":"preEnvelope"}},"correlationId":"run-1"}).to_string() }).unwrap();
    project(&owner, &mut hub);
    store.append(RunEvent { run_id: stream, kind: "run.completed".into(), payload: json!({"endedAt":"2026-10-21T20:00:00Z","status":"success","summary":"Durable completion"}).to_string() }).unwrap();
    project(&owner, &mut hub);
    let snapshot = hub
        .snapshot("runEvidence", &key_for("runEvidence", "run-1"))
        .unwrap();
    assert_eq!(snapshot.snapshot[0]["evidence"]["authority"], "durable");
    assert_eq!(
        snapshot.snapshot[0]["evidence"]["header"]["summary"],
        "Durable completion"
    );
    assert!(hub
        .subscribe(
            "runEvidence",
            &key_for("runEvidence", "run-1"),
            Some(snapshot.rev - 1)
        )
        .unwrap()
        .deltas
        .is_some());
    for i in 0..260 {
        run_store::append_to(&binding.jsonl, report("run-1", &format!("Result {i}")));
        project(&owner, &mut hub);
    }
    assert!(hub
        .subscribe("runEvidence", &key_for("runEvidence", "run-1"), Some(0))
        .unwrap()
        .snapshot
        .is_some());
}

#[test]
fn mcp_and_ui_boundaries_use_the_shared_assertion_writers() {
    let handler = include_str!("../../mcp/handler.rs");
    let body = handler
        .split("async fn wv_post_assertion(")
        .nth(1)
        .unwrap()
        .split("#[tool(")
        .next()
        .unwrap();
    assert!(body.contains("commands::weft_ledger::post_assertion("));
    assert!(body.contains("emit(\"mcp:wv-assertion-posted\", &assertion)"));
    let commands = include_str!("../../commands/weft_ledger.rs");
    let body = commands
        .split("async fn settle_assertion(")
        .nth(1)
        .unwrap()
        .split("match outcome")
        .next()
        .unwrap();
    assert!(body.contains("settle_assertion_row(&store, assertion_id, target)"));
    assert!(include_str!("../mod.rs").contains("let flags = next_updates(&handle).await;"));
}

#[tokio::test]
async fn committed_writers_wake_the_production_worker_and_emit_subscription_deltas() {
    use tauri::Listener;
    let home = tempfile::tempdir().unwrap();
    let binding = binding(home.path());
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(LiveHub::default());
    let hub = app.state::<LiveHub>();
    observe(&hub.run_review, &hub.wake, &binding);
    for topic in TOPICS {
        hub.run_review
            .lock()
            .unwrap()
            .prepare(
                topic,
                &key_for(topic, "run-1"),
                binding.clone(),
                "SW-A",
                "run-1",
                topic,
            )
            .unwrap();
    }
    refresh(app.handle()).unwrap();
    for topic in TOPICS {
        super::super::subscribe(
            app.handle(),
            topic,
            &key_for(topic, "run-1"),
            None,
            Some(topic),
            None,
        )
        .unwrap();
    }
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    app.listen("codesense:subscription-delta", move |event| {
        send.send(serde_json::from_str::<Value>(event.payload()).unwrap())
            .unwrap();
    });
    let store = crate::weft::WeftStore::open(&binding.weft).unwrap();
    let row = weft_ledger::post_assertion(&store, "worker", Some("run-1"), "Ready", "review", None)
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::next_updates(app.handle()),
    )
    .await
    .expect("the committed post wakes the real worker");
    let posted = receive.try_recv().unwrap();
    assert_eq!(posted["subscriptionId"], "runEndorsements");
    assert_eq!(posted["delta"]["upserts"][0]["assertion"]["id"], row.id);
    run_store::append_to(&binding.jsonl, report("run-1", "Worker result"));
    run_store::append_to(&binding.jsonl, outcome("run-1"));
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::next_updates(app.handle()),
    )
    .await
    .expect("completion wakes the real worker");
    let completed = receive.try_recv().unwrap();
    assert_eq!(completed["subscriptionId"], "runEvidence");
    assert_eq!(completed["delta"]["upserts"].as_array().unwrap().len(), 2);
    weft_ledger::settle_assertion_row(&store, row.id, "ratified").unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::next_updates(app.handle()),
    )
    .await
    .expect("settle wakes the real worker");
    let settled = receive.try_recv().unwrap();
    assert_eq!(
        settled["delta"]["removes"],
        json!([format!("assertion:{}", row.id)])
    );
}
