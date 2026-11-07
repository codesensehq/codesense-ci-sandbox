// SPDX-License-Identifier: MIT
use super::{
    admit::Outcome,
    discover::discover_routines,
    model::{EventSpec, MetadataCondition, RoutineTrigger, EVENT_METADATA_CHANGE},
    run_seams::{key, routine, save, store},
    test_support::{alice, TempHome, ORG, TEAM},
};
use crate::context_query::{
    engine::Viewer,
    index::IndexRoot,
    model::Layer,
    service::{ContextIndexService, Footprint},
};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[test]
fn file_worker_matches_admits_and_retains_coalesced_context() {
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let root = temp.root().join("observed");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("a.md");
    let content = |status: &str| {
        format!("---\ntype: story\nstory_id: A-1\nstatus: {status}\nreview_requested: false\n---\n")
    };
    std::fs::write(&path, content("planning")).unwrap();
    let mut def = routine("metadata", "0 9 * * *");
    def.triggers = vec![RoutineTrigger::Event(EventSpec {
        kind: EVENT_METADATA_CHANGE.into(),
        from_agent: None,
        role: None,
        metadata: Some(
            serde_json::from_value::<MetadataCondition>(serde_json::json!({
                "selection": {"where": {"kinds": ["story"]}},
                "target": {"key":"status","op":"eq","value":"in-progress"}
            }))
            .unwrap(),
        ),
    })];
    let mut boolean_trigger = def.triggers[0].clone();
    if let RoutineTrigger::Event(event) = &mut boolean_trigger {
        let condition = event.metadata.as_mut().unwrap();
        condition.target.key = "review_requested".into();
        condition.target.value = Some(serde_json::json!(true));
    }
    def.triggers.push(boolean_trigger);
    save(&home, def);
    let routines = discover_routines(&home, &alice()).unwrap();
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let test_home = home.clone();
    let worker_root = root.clone();
    let index = ContextIndexService::new();
    index.start_observed(
        Arc::new(move || {
            Some(Footprint {
                owner: 1,
                roots: vec![IndexRoot {
                    dir: worker_root.clone(),
                    org: Some(ORG.into()),
                    team: Some(TEAM.into()),
                    layer: Layer::Team,
                }],
                summary_headings: vec![],
            })
        }),
        Arc::new(|_| {}),
        Arc::new(move |change| {
            let result = super::metadata::offer(
                &(test_home.clone(), alice()),
                &routines,
                &store(&test_home),
                &change,
                &Viewer::default(),
                super::finish::now_ms(),
            )
            .unwrap();
            tx.lock().unwrap().send(result).unwrap();
        }),
    );
    let receive = || rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(receive().is_empty(), "first discovery is a baseline");
    std::fs::write(&path, content("in-progress")).unwrap();
    index.note_paths([path.clone()]);
    let fired = receive();
    assert_eq!(fired[0].1.outcome, Outcome::Queued);
    let run_id = &fired[0].1.run_id;
    assert_eq!(
        store(&home)
            .run(run_id)
            .unwrap()
            .unwrap()
            .trigger_payload
            .unwrap()["story_id"],
        "A-1"
    );
    store(&home)
        .transaction(|tx| {
            tx.execute(
                "UPDATE routine_runs SET session_id='wake-target' WHERE run_id=?1",
                [run_id],
            )?;
            Ok(())
        })
        .unwrap();
    std::fs::write(&path, content("blocked")).unwrap();
    index.note_paths([path.clone()]);
    assert!(receive().is_empty());
    std::fs::write(&path, content("in-progress")).unwrap();
    index.note_paths([path.clone()]);
    let reentry = receive();
    assert!(matches!(reentry[0].1.outcome, Outcome::Coalesced { .. }));
    assert_eq!(
        store(&home)
            .run(&reentry[0].1.run_id)
            .unwrap()
            .unwrap()
            .trigger_payload
            .unwrap()["previous_value"],
        "blocked"
    );
    let context =
        super::service::context_for_session_run(&home, "wake-target", Some(run_id)).unwrap();
    assert_eq!(
        context.trigger_payload.as_ref().unwrap()["previous_value"],
        "planning"
    );
    assert_eq!(context.coalesced_payloads[0]["previous_value"], "blocked");
    assert_eq!(context.coalesced_payloads[0]["story_id"], "A-1");
    assert_eq!(store(&home).runs(&key("metadata")).unwrap().len(), 2);
    std::fs::write(&path, "---\nstatus: [unfinished\n").unwrap();
    index.note_paths([path.clone()]);
    assert!(receive().is_empty());
    std::fs::write(&path, content("in-progress")).unwrap();
    index.note_paths([path.clone()]);
    assert!(receive().is_empty(), "repairing a parse is a baseline");
    index.reset();
    assert!(
        receive().is_empty(),
        "reset does not replay matching values"
    );
    assert!(
        super::service::context_for_session_run(&home, "another-session", Some(run_id)).is_err()
    );
    std::fs::write(&path, content("planning")).unwrap();
    index.note_paths([path.clone()]);
    assert!(receive().is_empty());
    std::fs::write(
        &path,
        content("in-progress").replace("review_requested: false", "review_requested: true"),
    )
    .unwrap();
    index.note_paths([path]);
    let both = receive();
    assert_eq!(
        both.len(),
        2,
        "different fields on the same document are independent occurrences"
    );
    let context =
        super::service::context_for_session_run(&home, "wake-target", Some(run_id)).unwrap();
    assert_eq!(context.coalesced_payloads.len(), 3);
}

#[test]
fn shared_predicates_scope_baseline_duplicates_and_capped_paths() {
    use crate::context_query::{
        engine::{self, DiskBodyReader},
        index::IndexState,
        model::ContextQuery,
        service::{IndexChange, SnapshotChange},
    };
    use serde_json::json;
    let temp = TempHome::new();
    let home = temp.codesense_home();
    let root = temp.root().join("selection");
    std::fs::create_dir_all(&root).unwrap();
    let roots = vec![IndexRoot {
        dir: root.clone(),
        org: Some(ORG.into()),
        team: Some(TEAM.into()),
        layer: Layer::Team,
    }];
    let viewer = Viewer::default();
    let cases = [
        (
            "status",
            json!("in-progress"),
            "status: planning",
            "status: in-progress",
            true,
        ),
        (
            "status",
            json!("in-progress"),
            "status: blocked",
            "status: in-progress",
            true,
        ),
        (
            "status",
            json!("in-progress"),
            "status: in-progress",
            "status: in-progress\ntitle: edited",
            false,
        ),
        (
            "reviewRequested",
            json!("true"),
            "review_requested: false",
            "review_requested: true",
            true,
        ),
        (
            "reviewRequested",
            json!(true),
            "title: absent",
            "review-requested: true",
            true,
        ),
        ("points", json!(1), "points: '01'", "points: 1.0", false),
        ("points", json!("1"), "points: '01'", "points: 1", true),
        (
            "tags",
            json!("blocked"),
            "tags: [a]",
            "tags: [a, blocked]",
            true,
        ),
        (
            "tags",
            json!("blocked"),
            "tags: [blocked, a]",
            "tags: [a, blocked]",
            false,
        ),
    ];
    for (n, (field, target, before, after, expected)) in cases.into_iter().enumerate() {
        let id = format!("case-{n}");
        let condition: MetadataCondition = serde_json::from_value(json!({"selection":{"where":{"kinds":["story"]}}, "target":{"key":field,"op":"eq","value":target}})).unwrap();
        let mut def = routine(&id, "0 9 * * *");
        def.triggers = vec![RoutineTrigger::Event(EventSpec {
            kind: EVENT_METADATA_CHANGE.into(),
            from_agent: None,
            role: None,
            metadata: Some(condition.clone()),
        })];
        save(&home, def);
        let routines = discover_routines(&home, &alice())
            .unwrap()
            .into_iter()
            .filter(|r| r.key.id == id)
            .collect::<Vec<_>>();
        let path = root.join(format!("{id}.md"));
        let doc = |body: &str| format!("---\ntype: story\nstory_id: {id}\n{body}\n---\n");
        std::fs::write(&path, doc(before)).unwrap();
        let mut state = IndexState::default();
        state.reconcile(roots.clone());
        let previous = Arc::new(state.snapshot(1, true));
        std::fs::write(&path, doc(after)).unwrap();
        state.refresh_paths(&[path.clone()]);
        let current = Arc::new(state.snapshot(2, true));
        let change = SnapshotChange {
            observer_id: format!("case-{n}"),
            owner: Some(1),
            epoch: 0,
            previous,
            current,
            change: IndexChange {
                generation: 2,
                paths: vec![path.to_string_lossy().into_owned()],
                orgs: vec![ORG.into()],
                all_orgs: false,
                truncated: false,
            },
            replaced: false,
        };
        let admissions = super::metadata::offer(
            &(home.clone(), alice()),
            &routines,
            &store(&home),
            &change,
            &viewer,
            super::finish::now_ms(),
        )
        .unwrap();
        assert_eq!(!admissions.is_empty(), expected, "{id}");
        // The trigger's target agrees with the public query over identical files.
        let query = ContextQuery {
            filters: vec![condition.target.clone()],
            ..condition.selection.clone()
        };
        let queried = change
            .current
            .query(&query, &viewer, &DiskBodyReader)
            .unwrap();
        let record = change
            .current
            .records
            .iter()
            .find(|r| r.path == path.to_string_lossy())
            .unwrap();
        assert_eq!(
            queried.rows.iter().any(|r| r.path == record.path),
            engine::clause_matches(record, &condition.target, &viewer)
        );
        let again = super::metadata::offer(
            &(home.clone(), alice()),
            &routines,
            &store(&home),
            &change,
            &viewer,
            super::finish::now_ms(),
        )
        .unwrap();
        if expected {
            assert_eq!(again[0].1.outcome, Outcome::Duplicate);
        }
        let mut baseline = change.clone();
        baseline.replaced = true;
        assert!(super::metadata::offer(
            &(home.clone(), alice()),
            &routines,
            &store(&home),
            &baseline,
            &viewer,
            0
        )
        .unwrap()
        .is_empty());
    }
    let mut def = routine("bulk", "0 9 * * *");
    def.triggers=vec![RoutineTrigger::Event(EventSpec {kind:EVENT_METADATA_CHANGE.into(),from_agent:None,role:None,metadata:Some(serde_json::from_value(json!({"selection":{"where":{"pathPrefix":"bulk/"}},"target":{"key":"status","op":"eq","value":"in-progress"},"previous":{"key":"status","op":"eq","value":"planning"}})).unwrap())})];
    save(&home, def);
    let bulk = root.join("bulk");
    std::fs::create_dir_all(&bulk).unwrap();
    for n in 0..202 {
        std::fs::write(bulk.join(format!("{n}.md")), "---\nstatus: planning\n---\n").unwrap();
    }
    std::fs::write(
        bulk.join("html.html"),
        "<html><head><meta name=\"cs:status\" content=\"planning\"></head></html>",
    )
    .unwrap();
    std::fs::write(
        bulk.join("wrong-previous.md"),
        "---\nstatus: blocked\n---\n",
    )
    .unwrap();
    let mut state = IndexState::default();
    state.reconcile(roots);
    let previous = Arc::new(state.snapshot(20, true));
    let mut paths = vec![];
    for n in 0..202 {
        let p = bulk.join(format!("{n}.md"));
        std::fs::write(&p, "---\nstatus: in-progress\n---\n").unwrap();
        paths.push(p);
    }
    let html = bulk.join("html.html");
    std::fs::write(
        &html,
        "<html><head><meta name=\"cs:status\" content=\"in-progress\"></head></html>",
    )
    .unwrap();
    paths.push(html);
    let wrong_previous = bulk.join("wrong-previous.md");
    std::fs::write(&wrong_previous, "---\nstatus: in-progress\n---\n").unwrap();
    paths.push(wrong_previous);
    let discovered = bulk.join("discovered.md");
    std::fs::write(&discovered, "---\nstatus: in-progress\n---\n").unwrap();
    paths.push(discovered);
    state.refresh_paths(&paths);
    let change = SnapshotChange {
        observer_id: "bulk".into(),
        owner: Some(1),
        epoch: 0,
        previous,
        current: Arc::new(state.snapshot(21, true)),
        change: IndexChange {
            generation: 21,
            paths: paths
                .iter()
                .take(200)
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            orgs: vec![ORG.into()],
            all_orgs: false,
            truncated: true,
        },
        replaced: false,
    };
    let routines = discover_routines(&home, &alice())
        .unwrap()
        .into_iter()
        .filter(|r| r.key.id == "bulk")
        .collect::<Vec<_>>();
    let offered = super::metadata::offer(
        &(home.clone(), alice()),
        &routines,
        &store(&home),
        &change,
        &viewer,
        super::finish::now_ms(),
    )
    .unwrap();
    assert_eq!(
        offered.len(),
        203,
        "capped notifications still compare every changed record"
    );
    assert_eq!(
        offered
            .iter()
            .filter(|(_, a)| matches!(a.outcome, Outcome::Coalesced { .. }))
            .count(),
        202,
        "no agent-done rate limit"
    );
    assert_eq!(
        store(&home)
            .coalesced_payloads(&offered[0].1.run_id)
            .unwrap()
            .len(),
        202
    );
    // A different displayed team is not an observer input; reach is the routine's key.
    let mut other = routines[0].clone();
    other.key.team = "elsewhere".into();
    assert!(super::metadata::offer(
        &(home.clone(), alice()),
        &[other],
        &store(&home),
        &change,
        &viewer,
        0
    )
    .unwrap()
    .is_empty());
}

fn metadata_routine(id: &str, target: &str) -> super::model::RoutineDef {
    let mut def = routine(id, "0 9 * * *");
    def.triggers = vec![RoutineTrigger::Event(EventSpec {
        kind: EVENT_METADATA_CHANGE.into(),
        from_agent: None,
        role: None,
        metadata: Some(
            serde_json::from_value(serde_json::json!({
                "selection": {},
                "target": {"key": "status", "op": "eq", "value": target}
            }))
            .unwrap(),
        ),
    })];
    def
}

#[test]
fn ticker_uses_the_observed_profile_before_the_account_cache_expires() {
    use super::{
        model::Author,
        service::{self, PersonalOwner, Placement, SaveTarget},
        test_support::{agent, save_personal_agent, write_org, FakeSessions},
        tick::{RoutineAdmitter, RoutineTicker, DISCOVERY_TTL_MS},
    };
    use crate::domain::Principal;
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::SeqCst};

    let temp = TempHome::new();
    let home = temp.codesense_home();
    let mut keys = vec![];
    for owner in [1, 2] {
        let principal = Principal::from(owner);
        if owner == 2 {
            write_org(&home, owner);
        }
        save_personal_agent(
            &home,
            &principal,
            agent("default", codesense_pty_core::agent_cli::AgentCli::Claude),
        );
        let saved = service::save(
            &home,
            &principal,
            SaveTarget::Create(Placement::Personal(PersonalOwner {
                github_user_id: owner,
                username: format!("profile-{owner}"),
                display_name: format!("Profile {owner}"),
                avatar_url: None,
                org: ORG.into(),
            })),
            metadata_routine("private-transition", "in-progress"),
            &Author::user(principal.as_str()),
        )
        .unwrap();
        keys.push(saved.key);
        let root = temp.root().join(format!("profile-{owner}/private"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("story.md"), "---\nstatus: planning\n---\n").unwrap();
    }
    let selected = Arc::new(AtomicU64::new(1));
    let clock = Arc::new(AtomicI64::new(super::finish::now_ms()));
    let ticker = RoutineTicker::new(
        {
            let clock = Arc::clone(&clock);
            Arc::new(move || clock.load(SeqCst))
        },
        Arc::new(RoutineAdmitter),
        {
            let selected = Arc::clone(&selected);
            let home = home.clone();
            Arc::new(move || Some((home.clone(), Principal::from(selected.load(SeqCst)))))
        },
        Arc::new(FakeSessions::default()),
    );
    let cached_at = clock.load(SeqCst);
    ticker.tick().unwrap(); // Cache A, as the normal scheduler does.
    let index = ContextIndexService::new();
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    index.start_observed(
        {
            let selected = Arc::clone(&selected);
            let root = temp.root().to_path_buf();
            Arc::new(move || {
                let owner = selected.load(SeqCst);
                Some(Footprint {
                    owner,
                    roots: vec![IndexRoot {
                        dir: root.join(format!("profile-{owner}/private")),
                        org: Some(ORG.into()),
                        team: None,
                        layer: Layer::Private,
                    }],
                    summary_headings: vec![],
                })
            })
        },
        Arc::new(|_| {}),
        Arc::new(move |change| tx.lock().unwrap().send(change).unwrap()),
    );
    let receive = || rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let viewer = Viewer::default();
    let baseline_a = receive();
    assert_eq!(baseline_a.owner, Some(1));
    assert!(ticker
        .offer_metadata(&index, &baseline_a, &viewer)
        .unwrap()
        .is_empty());
    let path_a = temp.root().join("profile-1/private/story.md");
    std::fs::write(&path_a, "---\nstatus: in-progress\n---\n").unwrap();
    index.note_paths([path_a]);
    let change_a = receive();
    let fired_a = ticker.offer_metadata(&index, &change_a, &viewer).unwrap();
    assert_eq!(fired_a.len(), 1);
    assert_eq!(fired_a[0].0, keys[0].to_string());

    // B's baseline and real edit happen one millisecond into A's cache TTL.
    selected.store(2, SeqCst);
    clock.fetch_add(1, SeqCst);
    assert!(clock.load(SeqCst) - cached_at < DISCOVERY_TTL_MS);
    index.reset();
    let baseline_b = receive();
    assert_eq!(baseline_b.owner, Some(2));
    assert!(ticker
        .offer_metadata(&index, &baseline_b, &viewer)
        .unwrap()
        .is_empty());
    let path_b = temp.root().join("profile-2/private/story.md");
    std::fs::write(&path_b, "---\nstatus: in-progress\n---\n").unwrap();
    index.note_paths([path_b.clone()]);
    let change_b = receive();
    let fired_b = ticker.offer_metadata(&index, &change_b, &viewer).unwrap();
    assert_eq!(fired_b.len(), 1);
    assert_eq!(fired_b[0].0, keys[1].to_string());
    assert_eq!(fired_b[0].1.outcome, Outcome::Queued);
    let run = store(&home).run(&fired_b[0].1.run_id).unwrap().unwrap();
    assert_eq!(
        run.trigger_payload.unwrap()["path"],
        path_b.to_string_lossy().as_ref()
    );
    assert_eq!(
        store(&home).runs(&keys[0]).unwrap().len(),
        1,
        "B never admits under A"
    );
    assert_eq!(store(&home).runs(&keys[1]).unwrap().len(), 1);
    assert!(ticker
        .offer_metadata(&index, &change_a, &viewer)
        .unwrap()
        .is_empty());

    // Even an observation from the current epoch must match the admission principal.
    selected.store(1, SeqCst);
    assert!(index.is_current(&change_b));
    assert!(ticker
        .offer_metadata(&index, &change_b, &viewer)
        .unwrap()
        .is_empty());
    selected.store(2, SeqCst);
    index.reset();
    let baseline_b = receive();
    assert!(ticker
        .offer_metadata(&index, &baseline_b, &viewer)
        .unwrap()
        .is_empty());
    assert!(!index.is_current(&change_b));
    assert!(
        ticker
            .offer_metadata(&index, &change_b, &viewer)
            .unwrap()
            .is_empty(),
        "same-owner observations from before a reset are stale"
    );
}

#[test]
fn a_matching_condition_on_a_synced_duplicate_never_admits() {
    use super::{
        defs,
        test_support::{agent, copy_dir, save_team_agent},
    };

    let temp = TempHome::new();
    let home = temp.codesense_home();
    save_team_agent(
        &home,
        &alice(),
        agent("z-copy", codesense_pty_core::agent_cli::AgentCli::Claude),
    );
    save(&home, metadata_routine("duplicate", "done"));
    let winner = super::service::routine_dir_of(&home, &alice(), &key("duplicate")).unwrap();
    let duplicate = winner
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("z-copy/routines/duplicate");
    copy_dir(&winner, &duplicate);
    let mut def = defs::read_def(&duplicate).unwrap().unwrap();
    def.triggers = metadata_routine("duplicate", "in-progress").triggers;
    def.revision += 1;
    defs::write_current(&duplicate, &def).unwrap();
    let routines = discover_routines(&home, &alice()).unwrap();
    assert_eq!(routines.len(), 2);
    assert!(routines
        .iter()
        .any(|r| r.duplicate && r.dir == duplicate && r.held()));
    assert!(routines.iter().any(|r| !r.held() && r.dir == winner));

    let root = temp.root().join("observed");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("story.md");
    std::fs::write(&path, "---\nstatus: planning\n---\n").unwrap();
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let index = ContextIndexService::new();
    index.start_observed(
        Arc::new(move || {
            Some(Footprint {
                owner: 1,
                roots: vec![IndexRoot {
                    dir: root.clone(),
                    org: Some(ORG.into()),
                    team: Some(TEAM.into()),
                    layer: Layer::Team,
                }],
                summary_headings: vec![],
            })
        }),
        Arc::new(|_| {}),
        Arc::new(move |change| tx.lock().unwrap().send(change).unwrap()),
    );
    rx.recv_timeout(Duration::from_secs(10)).unwrap(); // Baseline.
    std::fs::write(&path, "---\nstatus: in-progress\n---\n").unwrap();
    index.note_paths([path]);
    let change = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let offered = super::metadata::offer(
        &(home.clone(), alice()),
        &routines,
        &store(&home),
        &change,
        &Viewer::default(),
        super::finish::now_ms(),
    )
    .unwrap();
    assert!(
        offered.is_empty(),
        "only the held duplicate's condition matches"
    );
    assert!(store(&home).runs(&key("duplicate")).unwrap().is_empty());
}
