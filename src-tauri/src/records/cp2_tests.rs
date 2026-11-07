// SPDX-License-Identifier: MIT
use super::binding::{execute, Binding};
use super::*;
use crate::dashboard_stream::frame::{
    FrameCallerKind, FrameGrant, FramePolicy, FrameStore, FrameTeam,
};
use crate::live_hub::projection::TopicRegistry;
use crate::verbs::{Decision, Dispatcher, Policy, Preset};
use serde_json::json;
use tempfile::{tempdir, TempDir};
fn fixture() -> (TempDir, Binding) {
    let tmp = tempdir().unwrap();
    let private = tmp.path().join("scratch/orgs/acme/ctx");
    let team = tmp.path().join("team");
    std::fs::create_dir_all(private.join("content/schema")).unwrap();
    std::fs::create_dir_all(team.join("content/schema")).unwrap();
    std::fs::write(team.join("content/schema/care.yaml"),"collection: care\nfolder: hq/data/care\nid: slug\ntitle: title\nfields:\n  lastDone: {type: date}\n").unwrap();
    let binding = Binding {
        caller: Default::default(),
        profile: "42".into(),
        org: "acme/ctx".into(),
        team: Some("family".into()),
        private_root: private,
        scratch_repo: tmp.path().join("scratch"),
        team_root: Some(team),
    };
    (tmp, binding)
}
/// Mints the way the host does: the slot and source scope pick the caller kind,
/// and the preset decides the grant through `Policy::effective_grant`.
fn make_frame(
    binding: Binding,
    slot: &str,
    source_scope: &str,
    preset: Preset,
) -> crate::dashboard_stream::frame::FrameRecord {
    let policy = Policy {
        preset,
        ..Policy::default()
    };
    let all = crate::verbs::capabilities::full();
    let kind = match (slot, source_scope) {
        ("message", _) => FrameCallerKind::Message,
        (_, "team") => FrameCallerKind::Team,
        _ => FrameCallerKind::Private,
    };
    let scope = match (preset, source_scope) {
        (Preset::Open, _) => "open",
        (_, "team" | "message") => "team",
        _ => "private",
    };
    let mut frame = FrameStore::default().mint_with_grant(
        9321,
        FrameGrant {
            frame_id: String::new(),
            principal: "human".into(),
            dashboard: "test".into(),
            slot: slot.into(),
            team: Some(FrameTeam {
                org: binding.org.clone(),
                team: binding.team.clone().unwrap(),
            }),
            scope: scope.into(),
        },
        "/ctx".into(),
        "/ctx/board.html".into(),
        "/ctx".into(),
        FramePolicy {
            team_safe: preset == Preset::TeamSafe,
            ..FramePolicy::default()
        },
        None,
        policy.effective_grant("test", slot, source_scope, &all, &all),
        kind,
    );
    frame.records_binding = Some(binding);
    frame
}
#[test]
fn cp2_notes_grant_matrix_keeps_private_records_in_private_panes_under_team_safe() {
    let (_tmp, binding) = fixture();
    let notes = json!({"collection":"notes","id":"n","body":"x"});
    for (slot, source, team_safe_allows) in [
        ("right-pane", "private", true),
        ("right-pane", "team", false),
        ("centre", "private", false),
        ("message", "message", false),
    ] {
        for preset in [Preset::Open, Preset::TeamSafe] {
            let frame = make_frame(binding.clone(), slot, source, preset);
            let policy = Policy {
                preset,
                ..Policy::default()
            };
            let expected = preset == Preset::Open || team_safe_allows;
            for verb in ["records.list", "records.write"] {
                let case = format!("{verb} in {slot}/{source} under {preset:?}");
                assert_eq!(
                    crate::verbs::check_frame_capability(&frame, verb, &notes).is_ok(),
                    expected,
                    "{case}"
                );
                assert_eq!(
                    matches!(
                        Dispatcher::default().dispatch_frame(
                            &frame,
                            verb,
                            notes.clone(),
                            &policy,
                            0
                        ),
                        Decision::Admit { .. }
                    ),
                    expected,
                    "{case}"
                );
            }
            assert_eq!(
                crate::verbs::check_frame_capability(
                    &frame,
                    "subscribe",
                    &json!({"topic":"records","collection":"notes"})
                )
                .is_ok(),
                expected,
                "subscribe in {slot}/{source} under {preset:?}"
            );
            if source != "private" {
                assert!(crate::verbs::check_frame_capability(
                    &frame,
                    "records.list",
                    &json!({"collection":"care"})
                )
                .is_ok());
            }
        }
    }
}
#[test]
fn cp2_story_declarations_use_team_content_and_team_reach_without_history() {
    let (_tmp, binding) = fixture();
    let root = binding.team_root.as_ref().unwrap();
    std::fs::write(
        root.join("content/schema/checks.yaml"),
        "collection: checks\nscope: story\nfolder: stories/epic/checks\nid: slug\n",
    )
    .unwrap();
    let store = binding.resolve("checks").unwrap();
    assert_eq!(store.collection.scope, Scope::Team);
    assert_eq!(
        store.collection.folder,
        std::path::PathBuf::from("content/stories/epic/checks")
    );
    assert_eq!(
        binding::capability(store.collection.scope, true),
        "records:write:team"
    );
    store
        .create(Some("tick"), &[], "done", &Actor::human(), None)
        .unwrap();
    assert!(root.join("content/stories/epic/checks/tick.md").exists());
    assert_eq!(
        store.history("tick", 20).unwrap_err(),
        "unavailable: no history for team collections"
    );
}
#[test]
fn cp2_record_budgets_are_per_frame_collection_refund_failures_and_exempt_flush() {
    let (_tmp, binding) = fixture();
    let frame = make_frame(binding.clone(), "right-pane", "private", Preset::Open);
    let mut dispatcher = Dispatcher::default();
    let policy = Policy::default();
    for i in 0..30 {
        assert!(matches!(
            dispatcher.dispatch_frame(
                &frame,
                "records.write",
                json!({"collection":"notes","id":"a"}),
                &policy,
                i
            ),
            Decision::Admit { .. }
        ));
    }
    assert!(matches!(
        dispatcher.dispatch_frame(
            &frame,
            "records.write",
            json!({"collection":"notes","id":"a"}),
            &policy,
            31
        ),
        Decision::Deny { .. }
    ));
    assert!(matches!(
        dispatcher.dispatch_frame(
            &frame,
            "records.write",
            json!({"collection":"care","id":"a"}),
            &policy,
            31
        ),
        Decision::Admit { .. }
    ));
    dispatcher.set_flushing(&frame.frame_id, true);
    assert!(matches!(
        dispatcher.dispatch_frame(
            &frame,
            "records.write",
            json!({"collection":"notes","id":"a"}),
            &policy,
            32
        ),
        Decision::Admit { .. }
    ));
    dispatcher.set_flushing(&frame.frame_id, false);
    let other = make_frame(binding, "right-pane", "private", Preset::Open);
    let Decision::Admit { call_id } = dispatcher.dispatch_frame(
        &other,
        "records.write",
        json!({"collection":"notes","id":"a"}),
        &policy,
        33,
    ) else {
        panic!()
    };
    dispatcher
        .settle(&call_id, false, "no such record")
        .unwrap();
    for i in 0..30 {
        assert!(matches!(
            dispatcher.dispatch_frame(
                &other,
                "records.write",
                json!({"collection":"notes","id":"a"}),
                &policy,
                34 + i
            ),
            Decision::Admit { .. }
        ));
    }
}
#[test]
fn cp2_transport_operations_keep_merge_receipts_and_human_only_delete() {
    let (_tmp, binding) = fixture();
    let actor = Actor::agent("Helper", None, None);
    let created = execute(
        &binding,
        "create",
        &json!({"collection":"notes","body":"base\n"}),
        &Actor::human(),
        None,
        None,
    )
    .unwrap();
    let id = created["id"].as_str().unwrap();
    let edit =
        json!({"collection":"notes","id":id,"body":"human\n","base":"base\n","requestId":"one"});
    execute(
        &binding,
        "write",
        &json!({"collection":"notes","id":id,"body":"agent\n"}),
        &actor,
        None,
        None,
    )
    .unwrap();
    let page = make_frame(binding.clone(), "right-pane", "private", Preset::Open);
    let merged = super::commands::execute_frame(&page, "write", &edit, None).unwrap();
    assert_eq!(merged["merged"], true);
    assert!(!merged["overlaps"].as_array().unwrap().is_empty());
    assert_eq!(
        merged,
        execute(&binding, "write", &edit, &Actor::human(), None, None).unwrap()
    );
    let params = crate::mcp::records_tools::RecordsParams {
        collection: Some("notes".into()),
        id: Some(id.into()),
        ..Default::default()
    };
    assert!(
        crate::mcp::records_tools::run(&binding, "delete", &params, &actor, None)
            .unwrap_err()
            .contains("only a human")
    );
    assert!(execute(
        &binding,
        "delete",
        &json!({"collection":"notes","id":id}),
        &Actor::human(),
        None,
        None
    )
    .is_ok());
    let created = execute(
        &binding,
        "create",
        &json!({"collection":"care","id":"tick","body":"Done"}),
        &actor,
        None,
        None,
    )
    .unwrap();
    assert!(created["commit"].is_null());
    assert!(execute(
        &binding,
        "delete",
        &json!({"collection":"care","id":"tick"}),
        &actor,
        None,
        None
    )
    .is_ok());
}
#[test]
fn cp2_projection_is_body_free_scoped_diffs_once_and_replays_or_resyncs() {
    let (_tmp, binding) = fixture();
    let store = binding.resolve("care").unwrap();
    let created = store
        .create(
            Some("tick"),
            &[("title".into(), json!("Care"))],
            "\nPreview\nTwo\nThree\nFour\nSecret body",
            &Actor::human(),
            None,
        )
        .unwrap();
    let row = projection::row(&store, &created.record);
    assert!(row.get("body").is_none());
    assert_eq!(row["preview"], "Preview · Two · Three · Four");
    let key = binding.view_key(&store);
    let mut topics = TopicRegistry::default();
    topics
        .subscribe_window("pane", "records", &key, None, None)
        .unwrap();
    let delta = topics
        .replace_records(
            &key,
            vec![row.clone()],
            Some(json!(Actor::human())),
            Some("create".into()),
            Some("one".into()),
        )
        .unwrap();
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].request_id.as_deref(), Some("one"));
    assert_eq!(topics.deliver(&delta).len(), 1);
    assert!(topics
        .replace_records(&key, vec![row], None, None, None)
        .unwrap()
        .is_empty());
    assert!(topics
        .snapshot("records", "records:team:else/ctx/family:care")
        .unwrap()
        .snapshot
        .is_empty());
    assert!(topics
        .snapshot("records", "records:team:acme/ctx/family:other")
        .unwrap()
        .snapshot
        .is_empty());
    assert_eq!(
        topics
            .subscribe("records", &key, Some(0))
            .unwrap()
            .deltas
            .unwrap()[0]
            .kind
            .as_deref(),
        Some("create")
    );
    assert!(topics
        .subscribe("records", &key, Some(99))
        .unwrap()
        .snapshot
        .is_some());
}

#[test]
fn cp2_audit_is_once_per_record_per_minute() {
    let mut audit = projection::AuditWindow::default();
    let folder = std::path::PathBuf::from("care");
    assert!(audit.admit(folder.clone(), "one", 0));
    assert!(audit.admit(folder.clone(), "two", 1));
    assert!(!audit.admit(folder.clone(), "one", 59_999));
    assert!(audit.admit(folder, "one", 60_000));
}
#[test]
fn cp2_records_projection_is_windowed_and_paged() {
    let mut topics = TopicRegistry::default();
    let key = "records:team:acme/ctx/family:care";
    let rows = (0..450)
        .map(|i| json!({"id":format!("{i:04}"),"order":i}))
        .collect();
    topics.replace_records(key, rows, None, None, None).unwrap();
    let first = topics
        .subscribe_window("paged", "records", key, None, None)
        .unwrap();
    assert_eq!(first.snapshot.unwrap().len(), 200);
    assert_eq!(first.total, Some(450));
    let next = topics
        .page("paged", first.cursor.as_deref().unwrap())
        .unwrap();
    assert_eq!(next.snapshot.len(), 200);
    let last = topics
        .page("paged", next.cursor.as_deref().unwrap())
        .unwrap();
    assert_eq!(last.snapshot.len(), 50);
    assert!(last.cursor.is_none());
}
#[test]
fn cp2_frame_scope_cannot_be_replaced_by_args_and_collections_are_grant_filtered() {
    let (_tmp, binding) = fixture();
    let page = make_frame(binding, "right-pane", "team", Preset::TeamSafe);
    let visible = super::commands::execute_frame(&page, "collections", &json!({}), None).unwrap();
    assert!(visible
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["scope"] == "team"));
    assert!(super::commands::execute_frame(
        &page,
        "create",
        &json!({"collection":"care","id":"filter","org":"another/org"}),
        None
    )
    .unwrap_err()
    .contains("scope is fixed"));
}

#[test]
fn cp2_configured_team_root_ignores_collection_index_roots_named_content() {
    use crate::context_query::{index::IndexRoot, model::Layer};
    let home = tempdir().unwrap();
    let org_root = crate::context_repo::context_attachments::tests::checkout(
        home.path(),
        "context-checkout",
        "acme/ctx",
    );
    crate::context_repo::context_attachments::attach(home.path(), &42u64.into(), &org_root)
        .unwrap();
    let roots = ["other", "family"].map(|team| {
        let root = org_root.join(team);
        std::fs::create_dir_all(root.join("content/schema")).unwrap();
        std::fs::write(
            root.join("content/schema/care.yaml"),
            "collection: care\nfolder: hq/data/content\nid: slug\n",
        )
        .unwrap();
        IndexRoot {
            dir: root.join("content"),
            org: Some("acme/ctx".into()),
            team: Some(team.into()),
            layer: Layer::Team,
        }
    });
    let shadow = org_root.join("family/hq/data/content");
    std::fs::create_dir_all(&shadow).unwrap();
    let mut index = crate::context_query::index::IndexState::default();
    index.reconcile(
        std::iter::once(IndexRoot {
            dir: shadow.clone(),
            org: Some("acme/ctx".into()),
            team: Some("family".into()),
            layer: Layer::Team,
        })
        .chain(roots)
        .collect(),
    );
    assert_eq!(index.snapshot(1, true).roots[0].dir, shadow);
    let binding =
        Binding::from_context(home.path(), &42u64.into(), "acme/ctx", Some("family")).unwrap();
    let other_profile =
        Binding::from_context(home.path(), &43u64.into(), "acme/ctx", Some("family")).unwrap();
    assert_eq!(
        binding.team_root.as_deref(),
        Some(org_root.join("family").as_path())
    );
    assert_ne!(binding.private_root, other_profile.private_root);
    let frames = FrameStore::default();
    let page = make_frame(binding.clone(), "right-pane", "team", Preset::TeamSafe);
    let minted = frames.mint(
        9321,
        page.grant,
        "/ctx".into(),
        "/ctx/board.html".into(),
        "/ctx".into(),
        FramePolicy::default(),
        None,
    );
    frames
        .bind_records(&minted.frame_id, binding.clone())
        .unwrap();
    let stored = frames.by_id(&minted.frame_id).unwrap();
    super::commands::execute_frame(
        &stored,
        "create",
        &json!({"collection":"care","id":"filter","body":"Bound"}),
        None,
    )
    .unwrap();
    assert!(org_root.join("family/hq/data/content/filter.md").exists());
    assert!(!org_root.join("other/hq/data/content/filter.md").exists());
    let mcp_binding =
        Binding::from_context(home.path(), &42u64.into(), "acme/ctx", Some("family")).unwrap();
    let params = crate::mcp::records_tools::RecordsParams {
        collection: Some("care".into()),
        id: Some("mcp-filter".into()),
        body: Some("Agent bound".into()),
        ..Default::default()
    };
    crate::mcp::records_tools::run(
        &mcp_binding,
        "create",
        &params,
        &Actor::agent("Helper", None, None),
        None,
    )
    .unwrap();
    assert!(org_root
        .join("family/hq/data/content/mcp-filter.md")
        .exists());

    binding
        .resolve("notes")
        .unwrap()
        .create(
            None,
            &[],
            "Private",
            &Actor::agent("Helper", None, None),
            None,
        )
        .unwrap();
    assert!(!other_profile.private_root.join("notes").exists());
}

#[test]
fn cp2_ambiguous_collection_names_are_refused_and_mcp_defaults_follow_session() {
    let (_tmp, binding) = fixture();
    std::fs::write(
        binding.private_root.join("content/schema/care.yaml"),
        "collection: care\nfolder: care\nid: slug\n",
    )
    .unwrap();
    assert!(binding
        .resolve("care")
        .err()
        .unwrap()
        .contains("ambiguous collection"));
    assert_eq!(
        crate::mcp::local_identity::session(None, Some("bound-session")).unwrap(),
        "bound-session"
    );
    assert!(crate::mcp::local_identity::session(Some("explicit"), Some("bound-session")).is_err());
    assert!(crate::mcp::local_identity::session(Some("explicit"), None).is_err());
    let mut params = crate::mcp::records_tools::RecordsParams::default();
    crate::mcp::records_tools::bind_defaults(
        &mut params,
        Some(&crate::commands::dashboard_launch::LaunchTeam {
            org: "acme/ctx".into(),
            team: "family".into(),
        }),
    );
    assert_eq!(params.org.as_deref(), Some("acme/ctx"));
    assert_eq!(params.team.as_deref(), Some("family"));
    params.org = Some("else/ctx".into());
    params.team = None;
    crate::mcp::records_tools::bind_defaults(
        &mut params,
        Some(&crate::commands::dashboard_launch::LaunchTeam {
            org: "acme/ctx".into(),
            team: "family".into(),
        }),
    );
    assert!(params.team.is_none());
}

#[test]
fn cp2_production_projection_callbacks_publish_mcp_outside_and_delayed_commits() {
    use std::sync::mpsc;
    use std::time::Duration;
    use tauri::{Emitter, Listener, Manager};
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(crate::live_hub::LiveHub::default());
    projection::start(app.handle());
    let (_tmp, binding) = fixture();
    let care = binding.resolve("care").unwrap();
    let care_key = projection::prepare(app.handle(), &binding, "care").unwrap();
    let notes_key = projection::prepare(app.handle(), &binding, "notes").unwrap();
    for (id, key) in [("pane-care", &care_key), ("pane-notes", &notes_key)] {
        crate::live_hub::subscribe(app.handle(), "records", key, None, Some(id), None).unwrap();
    }
    let (send, receive) = mpsc::channel();
    app.listen("codesense:subscription-delta", move |e| {
        let _ = send.send(serde_json::from_str::<serde_json::Value>(e.payload()).unwrap());
    });
    let (send, legacy) = mpsc::channel();
    app.listen(history::NOTES_CHANGED, move |e| {
        let _ = send.send(serde_json::from_str::<serde_json::Value>(e.payload()).unwrap());
    });
    let next = |key: &str, kind: &str| -> serde_json::Value {
        loop {
            let event = receive.recv_timeout(Duration::from_secs(3)).unwrap();
            if event["delta"]["viewKey"] == key && event["delta"]["kind"] == kind {
                return event["delta"].clone();
            }
        }
    };
    let params = crate::mcp::records_tools::RecordsParams {
        collection: Some("care".into()),
        id: Some("filter".into()),
        body: Some("Before".into()),
        request_id: Some("create".into()),
        ..Default::default()
    };
    let actor = Actor::agent("Helper", None, None);
    crate::mcp::records_tools::run(&binding, "create", &params, &actor, None).unwrap();
    let created = next(&care_key, "create");
    assert_eq!(created["requestId"], "create");
    assert_eq!(created["actor"], json!(actor));
    crate::mcp::records_tools::run(&binding, "create", &params, &actor, None).unwrap();
    assert!(receive.recv_timeout(Duration::from_millis(50)).is_err());

    std::fs::write(
        care.folder().unwrap().join("filter.md"),
        "---\npriority: high\n---\nOutside",
    )
    .unwrap();
    std::fs::write(
        binding
            .team_root
            .as_ref()
            .unwrap()
            .join("content/schema/care.yaml"),
        "collection: care\nfolder: hq/data/care\nid: slug\nfields:\n  priority: {type: text}\n",
    )
    .unwrap();
    app.emit(crate::context_query::service::CHANGE_EVENT, ())
        .unwrap();
    let outside = next(&care_key, "outside");
    assert_eq!(outside["upserts"][0]["preview"], "Outside");
    assert_eq!(outside["upserts"][0]["fields"]["priority"], "high");
    app.emit(crate::context_query::service::CHANGE_EVENT, ())
        .unwrap();
    assert!(receive.recv_timeout(Duration::from_millis(50)).is_err());

    let notes = binding.resolve("notes").unwrap();
    let note = notes
        .create(None, &[], "Original", &actor, Some("agent-create"))
        .unwrap();
    let agent_delta = next(&notes_key, "create");
    assert_eq!(agent_delta["requestId"], "agent-create");
    assert_eq!(agent_delta["actor"], json!(actor));
    let native = loop {
        let e = legacy.recv_timeout(Duration::from_secs(3)).unwrap();
        if e["id"] == note.record.id {
            break e;
        }
    };
    assert_eq!(native["orgSlug"], "acme/ctx");
    assert_eq!(native["source"], "records");
    notes
        .write(
            &note.record.id,
            &BodyEdit::Replace("Human edit".into()),
            &[],
            &Actor::human(),
            None,
        )
        .unwrap();
    let written = next(&notes_key, "write")["upserts"][0].clone();
    let human_native = loop {
        let event = legacy.recv_timeout(Duration::from_secs(3)).unwrap();
        if event["kind"] == "write" {
            break event;
        }
    };
    assert_eq!(human_native["source"], "records");
    assert_eq!(human_native["actor"]["kind"], "human");
    // Seeding another Notes store while this view is registered must not re-enter its lock.
    let other = binding.private_root.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    super::notes::ensure_prompts_seeded(&other);
    history::flush_human(&notes.location().unwrap()).unwrap();
    // The flush can commit seeded notes too; select the edited record by id.
    let mut committed = loop {
        let delta = next(&notes_key, "commit");
        if let Some(row) = delta["upserts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"].as_str() == Some(note.record.id.as_str()))
        {
            break row.clone();
        }
    };
    assert_ne!(committed["headCommit"], written["headCommit"]);
    committed["headCommit"] = written["headCommit"].clone();
    assert_eq!(committed, written);
}

#[test]
fn cp2_record_deltas_update_pagination_cursors() {
    let mut topics = TopicRegistry::default();
    let key = "records:team:acme/ctx/family:care";
    let mut rows = (0..200)
        .map(|i| json!({"id":format!("{i:04}"),"order":i}))
        .collect::<Vec<_>>();
    topics
        .replace_records(key, rows.clone(), None, None, None)
        .unwrap();
    assert!(topics
        .subscribe_window("cursor", "records", key, None, None)
        .unwrap()
        .cursor
        .is_none());
    rows.push(json!({"id":"last","order":200}));
    let changes = topics.replace_records(key, rows, None, None, None).unwrap();
    let delivered = topics.deliver(&changes);
    assert_eq!(delivered[0].1.cursor, Some(Some("records:next".into())));
    assert_eq!(
        topics
            .page("cursor", "records:next")
            .unwrap()
            .snapshot
            .len(),
        1
    );
    let moved = topics.move_window("cursor", [200, 201]).unwrap();
    assert_eq!(moved.cursor, Some(None));
    assert_eq!(
        topics.move_window("cursor", [0, 200]).unwrap().cursor,
        Some(Some("records:next".into()))
    );
}

#[test]
fn cp2_previews_match_the_native_notes_summary() {
    let body = "# Plan\n- [ ] Call the bank [@Jimmy](cs:agent:jimmy%23one)\n```js\nconst x = 1\n```\n> quoted **bold** [link](http://x)\n1. fifth line\n";
    let (preview, refs) = projection::summary(body);
    assert_eq!(preview, "Plan · Call the bank · js · const x = 1");
    assert_eq!(refs, vec![json!({"handle":"jimmy#one","name":"Jimmy"})]);
    let (preview, refs) = projection::summary(
        "---\n* [x]\u{a0}done _it_\n\n[@](cs:agent:a) [@B](cs:agent:a) [@C](cs:agent:%E0%A4%A)\n",
    );
    assert_eq!(preview, "done it");
    assert_eq!(refs, vec![json!({"handle":"a","name":"a"})]);
    let (preview, _) = projection::summary(&"a".repeat(300));
    assert_eq!(preview.chars().count(), 240);
    assert_eq!(projection::summary(""), (String::new(), vec![]));
}
