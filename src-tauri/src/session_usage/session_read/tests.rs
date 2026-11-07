// SPDX-License-Identifier: MIT
use super::*;
use crate::session_usage::{
    event::Tokens,
    record::{hand_started, SessionRecord},
    tests::{event, store},
    UsageStore,
};
use serde_json::json;

mod turns;

fn params(id: &str) -> Params {
    serde_json::from_value(json!({"sessionId":id})).unwrap()
}
async fn get(path: &Path, id: &str) -> Value {
    read(
        path,
        &path.with_file_name("conversations"),
        &params(id),
        &CallerScope::Human,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn native_ids_costs_and_reopen_in_both_arrival_orders() {
    for usage_first in [false, true] {
        let (dir, mut s) = store();
        let path = dir.path().join("usage.sqlite");
        let mut r = SessionRecord::new("tab");
        r.cli = Some("codex".into());
        s.write_session_record(&r).unwrap();
        s.register_launch("tab", "acme", Some("dev"), "a", "h", None)
            .unwrap();
        let e = event(
            "e",
            "native",
            "gpt-6-sol",
            1_790_000_000_000,
            Tokens {
                input: 100,
                ..Default::default()
            },
        );
        if usage_first {
            s.ingest(&[e.clone()]).unwrap();
        }
        // The same path the telemetry receiver uses to bind its native id.
        let owner = s.launch_owner("tab").unwrap().unwrap();
        s.attribute_if_absent("native", &owner, "tab").unwrap();
        s.attribute_if_absent("native", &owner, "tab").unwrap();
        if !usage_first {
            s.ingest(&[e]).unwrap();
        }
        drop(s);
        let _reopened = UsageStore::open(&path).unwrap();
        let a = get(&path, "tab").await;
        assert_eq!(a, get(&path, "native").await);
        assert_eq!(a["nativeSessionIds"], json!(["native"]));
        assert_eq!(a["models"][0]["calls"], 1);
        assert_eq!(a["cost"]["source"], "rate_table");
        assert_eq!(a["retries"], Value::Null);
        assert!(a["unavailable"]["retries"]
            .as_str()
            .unwrap()
            .contains("no retry signal"));
    }
}

#[tokio::test]
async fn reported_rate_mixed_unpriced_and_zero_are_stored_call_facts() {
    let (dir, mut s) = store();
    let path = dir.path().join("usage.sqlite");
    for (id, model, reported, input) in [
        ("reported", "custom", Some(2.0), 10),
        ("rate", "gpt-6-sol", None, 10),
        ("unpriced", "no-price", None, 10),
        ("zero", "no-price", None, 0),
    ] {
        let mut e = event(
            id,
            id,
            model,
            1_790_000_000_000,
            Tokens {
                input,
                ..Default::default()
            },
        );
        e.reported_cost_usd = reported;
        s.ingest(&[e]).unwrap();
    }
    let mut e = event(
        "mixed-1",
        "mixed",
        "gpt-6-sol",
        1_790_000_000_000,
        Tokens {
            input: 10,
            ..Default::default()
        },
    );
    e.reported_cost_usd = Some(1.0);
    s.ingest(&[e.clone()]).unwrap();
    e.event_id = "mixed-2".into();
    e.reported_cost_usd = None;
    s.ingest(&[e]).unwrap();
    assert_eq!(get(&path, "reported").await["cost"]["source"], "reported");
    assert_eq!(get(&path, "rate").await["cost"]["source"], "rate_table");
    let mixed = get(&path, "mixed").await;
    assert_eq!(mixed["cost"]["source"], "mixed");
    assert_eq!(mixed["models"][0]["costSource"], "mixed");
    assert_eq!(mixed["models"][0]["tokens"]["input"], 20);
    assert_eq!(mixed["models"][0]["calls"], 2);
    let unknown = get(&path, "unpriced").await;
    assert_eq!(unknown["cost"]["totalUsd"], Value::Null);
    assert!(unknown["cost"]["reason"]
        .as_str()
        .unwrap()
        .contains("no-price"));
    assert_eq!(unknown["session"], Value::Null);
    assert_eq!(get(&path, "zero").await["cost"]["totalUsd"], 0.0);
}

#[tokio::test]
async fn hand_started_and_empty_usage_preserve_missing_facts_and_tool_errors() {
    use crate::session_usage::conversation::{ConversationMeta, IndexedCall, State};
    let (dir, mut s) = store();
    let path = dir.path().join("usage.sqlite");
    let record = hand_started("hand", Some("native"), Some("acme"), Some("dev"));
    s.write_session_record(&record).unwrap();
    let meta = ConversationMeta {
        session_id: "hand".into(),
        state: State::Present,
        source_format: None,
        bytes: 0,
        turn_count: 1,
        truncated: None,
        captured_at_ms: 1,
        unavailable: Default::default(),
    };
    let call = |seq, is_error| IndexedCall { aliases: Vec::new(),
        turn_index: 0,
        seq,
        native_id: None,
        name: "Bash".into(),
        mcp_server: None,
        is_error,
        ts_ms: None,
        skill: None,
    };
    s.write_conversation(
        &meta,
        &[call(0, Some(true)), call(1, Some(false)), call(2, None)],
    )
    .unwrap();
    use crate::session_usage::timing::{Signal, Stamped, TimingState};
    use codesense_pty_core::activity::Event;
    let mut timing = TimingState::default();
    for (at, signal) in [
        (100, Signal::Activity(Event::Submitted, "fixture")),
        (200, Signal::Activity(Event::Stopped, "fixture")),
    ] {
        timing
            .apply(
                &s,
                &Stamped {
                    session_id: "hand".into(),
                    at_ms: at,
                    signal,
                },
            )
            .unwrap();
    }
    let result = get(&path, "hand").await;
    assert_eq!(result["session"]["unavailable"], json!(record.unavailable));
    assert_eq!(result["session"]["timing"]["activeMs"], 100);
    assert_eq!(result["cost"]["totalUsd"], Value::Null);
    assert_eq!(result["cost"]["reason"], "no usage events for this session");
    assert_eq!(result["tools"]["used"][0]["calls"], 3);
    assert_eq!(result["tools"]["used"][0]["errors"], 1);
    assert_eq!(result["tools"]["used"][0]["errorUnknown"], 1);
    assert_eq!(result["run"]["state"], "unlinked");
    assert!(
        read(&path, dir.path(), &params("missing"), &CallerScope::Human)
            .await
            .unwrap_err()
            .contains("unknown session")
    );
}

#[tokio::test]
async fn conversation_states_paging_and_jumps_are_the_existing_pager() {
    use crate::session_conversation::StoredConversation;
    use crate::session_usage::conversation::{ConversationMeta, IndexedCall, State, Truncation};
    use codesense_pty_core::agent_cli::output::{SessionTranscript, Turn};
    for state in [
        None,
        Some(State::Present),
        Some(State::Unavailable),
        Some(State::Truncated),
        Some(State::Expired),
        Some(State::Unsupported),
    ] {
        let (dir, mut s) = store();
        let path = dir.path().join("usage.sqlite");
        let conv = dir.path().join("conversations");
        let mut r = SessionRecord::new("tab");
        r.cli = Some("codex".into());
        s.write_session_record(&r).unwrap();
        if let Some(state) = state {
            let cut = (state == State::Truncated).then_some(Truncation {
                dropped_turns: None,
                dropped_turn_count: 0,
                dropped_bytes: 10,
                trimmed_fields: 1,
            });
            let meta = ConversationMeta {
                session_id: "tab".into(),
                state,
                source_format: None,
                bytes: 0,
                turn_count: 3,
                truncated: cut.clone(),
                captured_at_ms: 1,
                unavailable: Default::default(),
            };
            let call = IndexedCall { aliases: Vec::new(),
                turn_index: 1,
                seq: 0,
                native_id: Some("call-1".into()),
                name: "shell".into(),
                mcp_server: None,
                is_error: None,
                ts_ms: None,
                skill: None,
            };
            s.write_conversation(&meta, &[call]).unwrap();
            std::fs::create_dir_all(&conv).unwrap();
            let transcript = SessionTranscript::new(
                Some("tab".into()),
                (0..3)
                    .map(|i| Turn {
                        role: "assistant".into(),
                        text: Some(format!("turn {i}")),
                        ..Default::default()
                    })
                    .collect(),
                Default::default(),
            );
            let stored = StoredConversation {
                session_id: "tab".into(),
                source_format: "fixture".into(),
                captured_at_ms: 1,
                truncated: cut,
                transcript,
            };
            std::fs::write(conv.join("tab.json"), serde_json::to_vec(&stored).unwrap()).unwrap();
        }
        for fields in [
            json!({"limit":1}),
            json!({"cursor":1,"limit":1}),
            json!({"turn":1}),
            json!({"toolUseId":"call-1"}),
        ] {
            let mut args = json!({"sessionId":"tab","include":["conversation"]});
            args.as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let p: Params = serde_json::from_value(args).unwrap();
            let got = read(&path, &conv, &p, &CallerScope::Human).await.unwrap();
            let expected = crate::session_conversation::read_conversation_in(
                &path,
                &conv,
                "tab",
                &p.selector().unwrap().unwrap(),
            )
            .unwrap();
            assert_eq!(got["conversation"], json!(expected));
        }
        assert!(get(&path, "tab").await.get("conversation").is_none());
    }
}

#[tokio::test]
async fn a_file_change_item_id_jumps_to_the_enclosing_call_turn() {
    use crate::session_conversation::StoredConversation;
    use crate::session_usage::conversation::{ConversationMeta, IndexedCall, State};
    use codesense_pty_core::agent_cli::output::{SessionTranscript, Turn};
    let (dir, mut s) = store();
    let path = dir.path().join("usage.sqlite");
    let conv = dir.path().join("conversations");
    s.write_session_record(&SessionRecord::new("tab")).unwrap();
    let meta = ConversationMeta {
        session_id: "tab".into(),
        state: State::Present,
        source_format: None,
        bytes: 0,
        turn_count: 3,
        truncated: None,
        captured_at_ms: 1,
        unavailable: Default::default(),
    };
    let call = IndexedCall {
        aliases: vec!["exec-alias-1".into()],
        turn_index: 2,
        seq: 0,
        native_id: Some("call_enclosing".into()),
        name: "exec".into(),
        mcp_server: None,
        is_error: None,
        ts_ms: None,
        skill: None,
    };
    s.write_conversation(&meta, &[call]).unwrap();
    std::fs::create_dir_all(&conv).unwrap();
    let transcript = SessionTranscript::new(
        Some("tab".into()),
        (0..3)
            .map(|i| Turn {
                role: "assistant".into(),
                text: Some(format!("turn {i}")),
                ..Default::default()
            })
            .collect(),
        Default::default(),
    );
    let stored = StoredConversation {
        session_id: "tab".into(),
        source_format: "fixture".into(),
        captured_at_ms: 1,
        truncated: None,
        transcript,
    };
    std::fs::write(conv.join("tab.json"), serde_json::to_vec(&stored).unwrap()).unwrap();
    let read_with = |selector: Value| {
        let mut args = json!({"sessionId":"tab","include":["conversation"]});
        args.as_object_mut()
            .unwrap()
            .extend(selector.as_object().unwrap().clone());
        let p: Params = serde_json::from_value(args).unwrap();
        let (path, conv) = (path.clone(), conv.clone());
        async move { read(&path, &conv, &p, &CallerScope::Human).await.unwrap() }
    };
    let by_alias = read_with(json!({"toolUseId":"exec-alias-1"})).await;
    let by_call = read_with(json!({"toolUseId":"call_enclosing"})).await;
    let by_turn = read_with(json!({"turn":2})).await;
    // `lookup` echoes what was asked for; the page itself must be the same.
    let lookup = &by_alias["conversation"]["lookup"];
    assert_eq!(lookup["found"], true);
    assert_eq!(lookup["requested"], "exec-alias-1");
    assert_eq!(lookup["turnIndex"], by_call["conversation"]["lookup"]["turnIndex"]);
    let page = |mut v: Value| {
        v["conversation"].as_object_mut().unwrap().remove("lookup");
        v["conversation"].take()
    };
    let alias_page = page(by_alias.clone());
    assert_eq!(alias_page, page(by_call));
    assert_eq!(alias_page, page(by_turn));
}

#[test]
fn selectors_require_include_and_are_mutually_exclusive() {
    for extra in [
        json!({"turn":1}),
        json!({"include":["conversation"],"turn":1,"cursor":0}),
        json!({"include":["conversation"],"limit":201}),
        json!({"include":["conversation"],"toolUseId":""}),
    ] {
        let mut args = json!({"sessionId":"tab"});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(serde_json::from_value::<Params>(args)
            .unwrap()
            .selector()
            .is_err());
    }
}

#[tokio::test]
async fn attribution_without_a_record_joins_every_native_id_consistently() {
    let (dir, mut s) = store();
    let path = dir.path().join("usage.sqlite");
    s.register_launch("launch", "acme", Some("dev"), "a", "h", None)
        .unwrap();
    let owner = s.launch_owner("launch").unwrap().unwrap();
    for id in ["one", "two"] {
        s.attribute_if_absent(id, &owner, "launch").unwrap();
        s.ingest(&[event(
            id,
            id,
            "gpt-6-sol",
            1_790_000_000_000,
            Tokens {
                input: 10,
                ..Default::default()
            },
        )])
        .unwrap();
    }
    let expected = get(&path, "launch").await;
    assert_eq!(expected, get(&path, "one").await);
    assert_eq!(expected, get(&path, "two").await);
    assert_eq!(expected["nativeSessionIds"], json!(["one", "two"]));
    assert_eq!(expected["models"][0]["calls"], 2);
    assert_eq!(expected["session"], Value::Null);
}

#[tokio::test]
async fn tasks_embed_the_existing_helper_for_codesense_and_native_ids() {
    use crate::session_usage::task::TaskRecord;
    let (dir, s) = store();
    let path = dir.path().join("usage.sqlite");
    let mut record = SessionRecord::new("task-tab");
    record.native_session_id = Some("task-native".into());
    s.write_session_record(&record).unwrap();
    s.insert_task_if_absent(&TaskRecord::new("task-tab", "Ship the joined read"))
        .unwrap();
    s.insert_task_if_absent(&TaskRecord::new("review", "Review the result"))
        .unwrap();
    s.add_task_link("review", "session", "task-tab").unwrap();
    let expected = json!(s.tasks_for_session("task-tab").unwrap());
    let a = get(&path, "task-tab").await;
    assert_eq!(a["tasks"], expected);
    assert_eq!(a, get(&path, "task-native").await);
    assert_eq!(a["tasks"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn tasks_are_an_empty_array_when_the_session_has_none() {
    let (dir, s) = store();
    s.write_session_record(&SessionRecord::new("empty-task-tab"))
        .unwrap();
    assert_eq!(
        get(&dir.path().join("usage.sqlite"), "empty-task-tab").await["tasks"],
        json!([])
    );
}

#[tokio::test]
async fn tools_used_absent_is_not_confirmed_zero() {
    let (dir, s) = store();
    s.write_session_record(&SessionRecord::new("tab")).unwrap();
    let got = get(&dir.path().join("usage.sqlite"), "tab").await;
    assert_eq!(got["tools"]["usedState"], "absent");
    assert_eq!(got["tools"]["used"], Value::Null);
    assert!(got["tools"]["usedReason"].as_str().unwrap().contains("no conversation"));
}

#[tokio::test]
async fn tools_used_unsupported_preserves_reader_reason() {
    use crate::session_usage::conversation::{ConversationMeta, State};
    let (dir, mut s) = store();
    let mut record = SessionRecord::new("tab");
    record.cli = Some("opencode".into());
    s.write_session_record(&record).unwrap();
    let before_capture = get(&dir.path().join("usage.sqlite"), "tab").await;
    assert_eq!(before_capture["tools"]["usedState"], "unsupported");
    assert!(before_capture["tools"]["usedReason"].as_str().unwrap().contains("opencode"));
    for (state, expected, reason) in [
        (State::Unsupported, "unsupported", "opencode has no reader"),
        (State::Unavailable, "unavailable", "capture has no native session id"),
    ] {
        s.write_conversation(&ConversationMeta {
            session_id: "tab".into(), state, source_format: None,
            bytes: 0, turn_count: 0, truncated: None, captured_at_ms: 1,
            unavailable: [("conversation".into(), reason.into())].into(),
        }, &[]).unwrap();
        let got = get(&dir.path().join("usage.sqlite"), "tab").await;
        assert_eq!(got["tools"]["usedState"], expected);
        assert_eq!(got["tools"]["used"], Value::Null);
        assert_eq!(got["tools"]["usedReason"], reason);
    }
}

#[tokio::test]
async fn tools_used_expired_retains_index_counts() {
    use crate::session_usage::conversation::{ConversationMeta, IndexedCall, State};
    let (dir, mut s) = store();
    s.write_session_record(&SessionRecord::new("tab")).unwrap();
    s.write_conversation(&ConversationMeta {
        session_id: "tab".into(), state: State::Expired, source_format: None,
        bytes: 0, turn_count: 1, truncated: None, captured_at_ms: 1,
        unavailable: Default::default(),
    }, &[IndexedCall { aliases: Vec::new(),
        turn_index: 0, seq: 0, native_id: None, name: "shell".into(),
        mcp_server: None, is_error: Some(false), ts_ms: None, skill: None,
    }]).unwrap();
    let got = get(&dir.path().join("usage.sqlite"), "tab").await;
    assert_eq!(got["tools"]["usedState"], "expired");
    assert_eq!(got["tools"]["used"][0]["calls"], 1);
    assert!(got["tools"]["used"][0]["firstRetainedError"].is_null());
    assert!(got["tools"]["usedReason"].as_str().unwrap().contains("index remains"));
}

#[tokio::test]
async fn tools_used_present_empty_is_confirmed_zero() {
    use crate::session_usage::conversation::{ConversationMeta, State};
    let (dir, mut s) = store();
    s.write_session_record(&SessionRecord::new("tab")).unwrap();
    s.write_conversation(&ConversationMeta {
        session_id: "tab".into(), state: State::Present, source_format: None,
        bytes: 0, turn_count: 0, truncated: None, captured_at_ms: 1,
        unavailable: Default::default(),
    }, &[]).unwrap();
    let got = get(&dir.path().join("usage.sqlite"), "tab").await;
    assert_eq!(got["tools"]["usedState"], "present");
    assert_eq!(got["tools"]["used"], json!([]));
    assert_eq!(got["tools"]["usedReason"], Value::Null);
}

#[tokio::test]
async fn review_trail_failure_location_comes_from_the_full_retained_tool_index() {
    use crate::session_usage::conversation::{ConversationMeta,IndexedCall,State};
    let (dir,mut store)=store();store.write_session_record(&SessionRecord::new("tab")).unwrap();
    store.write_conversation(&ConversationMeta{session_id:"tab".into(),state:State::Present,source_format:None,bytes:0,turn_count:100,truncated:None,captured_at_ms:1,unavailable:Default::default()},&[IndexedCall{aliases:Vec::new(),turn_index:22,seq:0,native_id:Some("failure-22".into()),name:"Edit".into(),mcp_server:None,is_error:Some(true),ts_ms:None,skill:None}, IndexedCall{aliases:Vec::new(),turn_index:90,seq:0,native_id:None,name:"Read".into(),mcp_server:None,is_error:Some(false),ts_ms:None,skill:None}, IndexedCall{aliases:Vec::new(),turn_index:99,seq:0,native_id:None,name:"shell".into(),mcp_server:None,is_error:None,ts_ms:None,skill:None}]).unwrap();
    let args=serde_json::from_value(json!({"sessionId":"tab"})).unwrap();let value=read(&dir.path().join("usage.sqlite"),&dir.path().join("conversations"),&args,&CallerScope::Human).await.unwrap();
    assert_eq!(value["tools"]["used"][0]["firstError"],json!({"turn":22,"toolUseId":"failure-22"}));
    assert!(value["tools"]["used"][1]["firstError"].is_null());
    assert!(value["tools"]["used"][2]["firstError"].is_null());assert_eq!(value["tools"]["used"][2]["errorUnknown"],1);
    std::fs::write(dir.path().join("story.md"),"Story").unwrap();
    store.link_story("tab",&crate::session_usage::StoryLink{story_id:"SV-FAILURE".into(),source:dir.path().join("story.md"),org:"acme".into()}).unwrap();
    let mut event=crate::session_usage::tests::event("call","tab","gpt-6-sol",1000,crate::session_usage::event::Tokens{input:1,..Default::default()});event.reported_cost_usd=Some(0.1);store.ingest(&[event]).unwrap();
    let params=serde_json::from_value(json!({"storyId":"SV-FAILURE"})).unwrap();
    let story=serde_json::to_value(crate::session_usage::story_changes::read(dir.path(),&params,&CallerScope::Human).await.unwrap()).unwrap();
    assert_eq!(story["sessions"][0]["tools"]["used"],value["tools"]["used"]);
}

#[tokio::test]
async fn review_trail_first_retained_failure_skips_only_the_dropped_interval() {
    use crate::session_usage::conversation::{ConversationMeta, IndexedCall, State, Truncation};
    for (dropped, expected) in [(Some((1, 99)), Some(150)), (None, Some(22)), (Some((1, 199)), None)] {
        let (dir, mut store) = store();
        store.write_session_record(&SessionRecord::new("tab")).unwrap();
        let calls = [22, 150].into_iter().map(|turn| IndexedCall { aliases: Vec::new(),
            turn_index: turn, seq: 0, native_id: Some(format!("failure-{turn}")),
            name: "Edit".into(), mcp_server: None, is_error: Some(true), ts_ms: None, skill: None,
        }).collect::<Vec<_>>();
        store.write_conversation(&ConversationMeta {
            session_id: "tab".into(), state: if dropped.is_some() { State::Truncated } else { State::Present },
            source_format: None, bytes: 0, turn_count: 200,
            truncated: dropped.map(|interval| Truncation { dropped_turns: Some(interval), dropped_turn_count: interval.1 - interval.0 + 1, dropped_bytes: 100, trimmed_fields: 0 }),
            captured_at_ms: 1, unavailable: Default::default(),
        }, &calls).unwrap();
        let value = get(&dir.path().join("usage.sqlite"), "tab").await;
        assert_eq!(value["tools"]["used"][0]["firstError"], json!({"turn":22,"toolUseId":"failure-22"}));
        let retained = expected.map(|turn| json!({"turn":turn,"toolUseId":format!("failure-{turn}")}));
        assert_eq!(value["tools"]["used"][0]["firstRetainedError"], json!(retained));
        assert_eq!(value["tools"]["used"][0]["errors"], 2);
    }
}
