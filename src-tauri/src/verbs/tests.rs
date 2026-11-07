// SPDX-License-Identifier: MIT
use super::*;
use serde_json::json;
#[test]
fn agent_list_actions_have_their_declared_contract() {
    let registry = Registry::default();
    for (name, class, grant, twin) in [
        (
            "moveWorkspace",
            Class::Prefs,
            "sessions:read",
            Some("reorder_workspace"),
        ),
        ("launchUnder", Class::Spawn, "sessions:read", None),
        ("openLauncher", Class::Spawn, "sessions:read", None),
        ("openRoutineDraft", Class::Spawn, "sessions:read", None),
        ("openAgentInbox", Class::Spawn, "inbox:read:team", None),
    ] {
        let spec = registry.get(name).expect(name);
        assert_eq!(registry.all().filter(|s| s.name == name).count(), 1);
        assert_eq!(spec.module, "agent");
        assert_eq!(spec.class, class);
        assert_eq!(spec.grant, grant);
        assert!(!spec.confirmable);
        assert!(spec.action);
        assert_eq!(spec.mcp_twin.as_deref(), twin);
        if twin.is_none() {
            assert!(spec.presentation_only_reason.is_some());
        }
    }
    let move_args = &registry.get("moveWorkspace").unwrap().args_schema;
    assert_eq!(move_args["required"], json!(["teamKey", "workspaceId"]));
    assert_eq!(
        move_args["oneOf"],
        json!([{"required":["before"]}, {"required":["after"]}])
    );
    for place in [json!({"before":"a"}), json!({"after":"a"})] {
        let mut args = json!({"teamKey":"acme:core","workspaceId":"b"});
        args.as_object_mut()
            .unwrap()
            .extend(place.as_object().unwrap().clone());
        assert!(jsonschema::is_valid(move_args, &args));
    }
    for args in [
        json!({"teamKey":"acme:core","workspaceId":"b"}),
        json!({"teamKey":"acme:core","workspaceId":"b","before":"a","after":"c"}),
    ] {
        assert!(!jsonschema::is_valid(move_args, &args));
    }
    assert_eq!(
        registry.get("gotoAgent").unwrap().args_schema["properties"]["preserveFocus"]["type"],
        "boolean"
    );
    for name in ["launchUnder", "openAgentInbox"] {
        assert_eq!(
            registry.get(name).unwrap().args_schema["required"],
            json!(["handle"])
        );
    }
}
fn caller(scope: &str) -> Caller {
    Caller {
        records_binding: None,
        kind: serde_json::from_value(json!(scope)).unwrap(),
        frame_id: "frame".into(),
        principal: "42".into(),
        dashboard: "Board".into(),
        slot: "center".into(),
        team: None,
        scope: scope.into(),
    }
}
#[test]
fn open_never_prompts_from_any_frame() {
    for scope in ["private", "builtin", "team", "message"] {
        let mut d = Dispatcher::default();
        for spec in Registry::default().all().filter(|s| s.confirmable) {
            let decision = d.dispatch(caller(scope), &spec.name, json!({}), &Policy::default(), 0);
            assert!(if spec.name == "loadMedia" {
                matches!(decision, Decision::Pending { .. })
            } else {
                matches!(decision, Decision::Admit { .. })
            });
            d = Dispatcher::default();
        }
    }
}
#[test]
fn pending_survives_caller_and_cancel_consumes_budget() {
    let mut d = Dispatcher::default();
    let mut p = Policy::default();
    p.switches.confirm_team_send_as_you = true;
    for _ in 0..10 {
        let Decision::Pending { confirm_id, .. } =
            d.dispatch(caller("team"), "sendToSession", json!({}), &p, 0)
        else {
            panic!("pending")
        };
        assert_eq!(
            d.dispatch(caller("team"), "sendToSession", json!({}), &p, 0),
            Decision::Deny {
                reason: "busy".into()
            }
        );
        assert!(d.answer(&confirm_id, false).is_ok());
    }
    assert_eq!(
        d.dispatch(caller("team"), "sendToSession", json!({}), &p, 0),
        Decision::Deny {
            reason: "denied: send budget".into()
        }
    );
}
#[test]
fn grant_precedes_budget_and_dialog() {
    let mut p = Policy::default();
    p.preset = Preset::TeamSafe;
    let mut d = Dispatcher::default();
    for _ in 0..11 {
        assert_eq!(
            d.dispatch(caller("message"), "stopSession", json!({}), &p, 0),
            Decision::Deny {
                reason: "denied: message frame".into()
            }
        );
    }
    assert!(d.pending.is_empty());
    assert!(d.windows.is_empty());
}
#[test]
fn failed_write_refunds_only_its_frame() {
    let mut d = Dispatcher::default();
    let p = Policy::default();
    for _ in 0..15 {
        let Decision::Admit { call_id } =
            d.dispatch(caller("private"), "setStatus", json!({}), &p, 0)
        else {
            panic!("admit")
        };
        d.settle(&call_id, false, "denied: read-only").unwrap();
    }
    for _ in 0..10 {
        assert!(matches!(
            d.dispatch(caller("private"), "setStatus", json!({}), &p, 0),
            Decision::Admit { .. }
        ));
    }
    assert!(matches!(
        d.dispatch(caller("private"), "setStatus", json!({}), &p, 0),
        Decision::Deny { .. }
    ));
    let mut other = caller("private");
    other.frame_id = "other".into();
    assert!(matches!(
        d.dispatch(other, "setStatus", json!({}), &p, 0),
        Decision::Admit { .. }
    ));
    assert!(matches!(
        d.dispatch(caller("private"), "setStatus", json!({}), &p, 10_000),
        Decision::Admit { .. }
    ));
}
#[test]
fn policy_switches_overrides_and_prompt_once() {
    let registry = Registry::default();
    let stop = registry.get("stopSession").unwrap();
    let send = registry.get("sendAsHuman").unwrap();
    let mut p = Policy::default();
    p.switches.confirm_message_destructive = true;
    assert_eq!(p.confirm(&caller("message"), stop), Confirm::Prompt);
    assert_eq!(p.confirm(&caller("team"), stop), Confirm::Never);
    p.switches.confirm_team_send_as_you = true;
    assert_eq!(p.confirm(&caller("team"), send), Confirm::Prompt);
    p.per_dashboard
        .entry("Board".into())
        .or_default()
        .insert("stopSession".into(), Confirm::Never);
    assert_eq!(p.confirm(&caller("message"), stop), Confirm::Never);
    p.per_dashboard
        .get_mut("Board")
        .unwrap()
        .insert("stopSession".into(), Confirm::PromptOnce);
    assert_eq!(p.confirm(&caller("private"), stop), Confirm::PromptOnce);
    assert_eq!(p.confirm(&caller("team"), stop), Confirm::Prompt);
    p.preset = Preset::TeamSafe;
    assert!(p.effective_switches().confirm_message_destructive);
    assert!(p.effective_switches().confirm_team_send_as_you);
}
#[test]
fn files_delete_and_move_are_open_by_default_and_confirm_gateable_per_dashboard() {
    let registry = Registry::default();
    for name in ["files.delete", "files.move"] {
        let spec = registry.get(name).unwrap();
        let mut p = Policy::default();
        for kind in ["team", "private", "builtin", "message"] {
            assert_eq!(p.confirm(&caller(kind), spec), Confirm::Never, "{name} {kind}");
        }
        p.per_dashboard
            .entry("Board".into())
            .or_default()
            .insert(name.into(), Confirm::Prompt);
        assert_eq!(p.confirm(&caller("team"), spec), Confirm::Prompt, "{name}");
        assert_eq!(p.confirm(&caller("private"), spec), Confirm::Prompt, "{name}");
        // A setting for one dashboard never reaches another dashboard's frame,
        // such as the native files tree.
        let mut tree = caller("builtin");
        tree.dashboard = "files-tree".into();
        assert_eq!(p.confirm(&tree, spec), Confirm::Never, "{name} native tree");
    }
}
#[test]
fn declined_files_delete_is_refused_and_never_admitted() {
    let mut p = Policy::default();
    p.per_dashboard
        .entry("Board".into())
        .or_default()
        .insert("files.delete".into(), Confirm::Prompt);
    let mut d = Dispatcher::default();
    let Decision::Pending { confirm_id, .. } = d.dispatch(
        caller("team"),
        "files.delete",
        json!({"path":"stories/a.md"}),
        &p,
        0,
    ) else {
        panic!("must prompt")
    };
    let saved = d.answer(&confirm_id, false).unwrap();
    assert_eq!(saved.verb, "files.delete");
    // A declined call is dropped: it cannot be settled or answered again, so
    // the host never runs the delete.
    assert!(d.settle(&saved.call_id, true, "confirmed").is_err());
    assert!(d.answer(&confirm_id, true).is_err());
}
#[test]
fn emit_registry() {
    let specs: Vec<_> = Registry::default().all().cloned().collect();
    let json = serde_json::to_string_pretty(&specs).unwrap() + "\n";
    if let Ok(path) = std::env::var("VERB_REGISTRY_OUTPUT") {
        std::fs::write(path, &json).unwrap();
    } else {
        assert_eq!(
            json,
            include_str!("registry.generated.json"),
            "run pnpm gen:verbs"
        );
    }
}
#[test]
fn registration_has_module_and_parity_explanation() {
    let r = Registry::default();
    for spec in r.all() {
        assert!(spec.mcp_twin.is_some() || spec.presentation_only_reason.is_some());
        assert!([
            "agent",
            "sessions",
            "inbox",
            "records",
            "processes",
            "commands",
            "files"
        ]
        .contains(&spec.module.as_str()));
    }
    for name in ["agents", "gotoAgent", "runPrompt", "sessionUsage"] {
        assert_eq!(r.get(name).unwrap().module, "agent");
    }
    let mut registry = Registry::default();
    let mut fixture = registry.get("sessions").unwrap().clone();
    fixture.name = "sessions.fixture".into();
    registry.register("sessions", fixture);
    assert_eq!(
        registry.get("sessions.fixture").unwrap().grant,
        "sessions:read"
    );
}

#[test]
fn send_prompt_uses_typed_send_policy_and_shared_budget() {
    let registry = Registry::default();
    let spec = registry.get("sendPrompt").unwrap();
    assert_eq!(spec.module, "agent");
    assert_eq!(spec.class, Class::Send);
    assert_eq!(spec.grant, "sessions:send");
    assert!(spec.confirmable && spec.audited && spec.action);
    assert!(spec.mcp_twin.is_none());
    assert_eq!(spec.presentation_only_reason.as_deref(), Some("Agents prompt running agents with send_to_session; this is the person's typed send from a page."));
    assert_eq!(spec.args_schema["required"], json!(["handle", "text"]));
    assert_eq!(spec.args_schema["properties"]["submit"]["type"], "boolean");
    assert!(super::policy::send_as_you("sendPrompt"));
    assert!(!super::capabilities::scope_allows("message", &spec.grant));
    let mut policy = Policy::default();
    assert_eq!(policy.confirm(&caller("team"), spec), Confirm::Never);
    policy.switches.confirm_team_send_as_you = true;
    assert_eq!(policy.confirm(&caller("team"), spec), Confirm::Prompt);
    policy.preset = Preset::TeamSafe;
    assert_eq!(policy.confirm(&caller("team"), spec), Confirm::Prompt);
    assert!(!policy.denied(&caller("team"), spec));

    let mut dispatcher = Dispatcher::default();
    for index in 0..10 {
        assert!(matches!(
            dispatcher.dispatch(
                caller("private"),
                if index % 2 == 0 {
                    "sendPrompt"
                } else {
                    "sendToSession"
                },
                json!({"handle":"agent", "text":"hello"}),
                &Policy::default(),
                0
            ),
            Decision::Admit { .. }
        ));
    }
    assert_eq!(
        dispatcher.dispatch(
            caller("private"),
            "sendPrompt",
            json!({}),
            &Policy::default(),
            0
        ),
        Decision::Deny {
            reason: "denied: send budget".into()
        }
    );
}
#[test]
fn contract_and_registry_cover_the_same_surface() {
    use crate::mcp::live_data_authoring::{ACTION_VERB_DOCS, READ_VERBS};
    let registry = Registry::default();
    let actions: std::collections::BTreeSet<_> = registry
        .all()
        .filter(|s| s.action)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(actions, ACTION_VERB_DOCS.iter().map(|s| s.verb).collect());
    let reads: std::collections::BTreeSet<_> = registry
        .all()
        .filter(|s| !s.action)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(reads, READ_VERBS.iter().copied().collect());
}
#[test]
fn unchanged_rate_lanes_and_cap_free_spawn_rules() {
    let p = Policy::default();
    for (verb, args, limit) in [
        ("sendToSession", json!({}), 10),
        ("runPrompt", json!({}), 10),
        ("setPrefs", json!({}), 30),
        ("openTab", json!({}), 10),
        ("openFile", json!({}), 10),
        ("openWorkspace", json!({"target":{"kind":"dir"}}), 10),
    ] {
        let mut d = Dispatcher::default();
        for _ in 0..limit {
            assert!(matches!(
                d.dispatch(caller("private"), verb, args.clone(), &p, 0),
                Decision::Admit { .. }
            ));
        }
        assert!(matches!(
            d.dispatch(caller("private"), verb, args.clone(), &p, 0),
            Decision::Deny { .. }
        ));
        assert!(matches!(
            d.dispatch(caller("private"), verb, args, &p, 10_000),
            Decision::Admit { .. }
        ));
    }
    let mut d = Dispatcher::default();
    for _ in 0..40 {
        assert!(matches!(
            d.dispatch(
                caller("private"),
                "openWorkspace",
                json!({"storyId":"S"}),
                &p,
                0
            ),
            Decision::Admit { .. }
        ));
    }
}
#[test]
fn policy_changes_apply_on_next_dispatch_and_arguments_cannot_bypass() {
    let mut p = Policy::default();
    let mut d = Dispatcher::default();
    assert!(matches!(
        d.dispatch(caller("team"), "sendToSession", json!({}), &p, 0),
        Decision::Admit { .. }
    ));
    p.switches.confirm_team_send_as_you = true;
    let Decision::Pending { confirm_id, .. } = d.dispatch(
        caller("team"),
        "sendToSession",
        json!({"confirm":false,"skipConfirm":true}),
        &p,
        0,
    ) else {
        panic!("must prompt")
    };
    let saved = d.answer(&confirm_id, true).unwrap();
    assert_eq!(saved.verb, "sendToSession");
    assert_eq!(saved.caller.dashboard, "Board");
    d.settle(&saved.call_id, true, "confirmed").unwrap();
    p.per_dashboard
        .entry("Board".into())
        .or_default()
        .insert("sendToSession".into(), Confirm::Never);
    assert!(matches!(
        d.dispatch(caller("team"), "sendToSession", json!({}), &p, 0),
        Decision::Admit { .. }
    ));
}
#[test]
fn team_safe_prompts_follow_frame_kind_table() {
    let p = Policy {
        preset: Preset::TeamSafe,
        ..Policy::default()
    };
    let registry = Registry::default();
    for name in [
        "stopSession",
        "closeWorkspace",
        "closeTerminalTab",
        "sendToSession",
        "sendToWorkspaceTab",
        "sendAsHuman",
        "openWorkspace",
    ] {
        assert_eq!(
            p.confirm(&caller("team"), registry.get(name).unwrap()),
            Confirm::Prompt
        );
        assert_eq!(
            p.confirm(&caller("private"), registry.get(name).unwrap()),
            Confirm::Never
        );
        assert_eq!(
            p.confirm(&caller("builtin"), registry.get(name).unwrap()),
            Confirm::Never
        );
    }
    let mut d = Dispatcher::default();
    assert!(matches!(
        d.dispatch(
            caller("team"),
            "openWorkspace",
            json!({"target":{"kind":"dir"}}),
            &p,
            0
        ),
        Decision::Admit { .. }
    ));
    let contract = super::contract();
    assert_eq!(contract["presets"], json!(["open", "team-safe"]));
    assert_eq!(
        contract["confirmable"],
        serde_json::to_value(
            registry
                .all()
                .filter(|s| s.confirmable)
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
        )
        .unwrap()
    );
}

#[test]
fn a_headed_directory_agent_still_requires_launch_confirmation() {
    let mut d = Dispatcher::default();
    let p = Policy {
        preset: Preset::TeamSafe,
        ..Policy::default()
    };
    assert!(matches!(
        d.dispatch(
            caller("team"),
            "openWorkspace",
            json!({"target":{"kind":"dir","path":"/work","agent":"builtin/codex"}}),
            &p,
            0
        ),
        Decision::Pending { .. }
    ));
}

#[test]
fn audit_targets_preserve_verb_specific_labels_without_payload_text() {
    let mut caller = caller("team");
    caller.team = Some(CallerTeam {
        org: "acme/widgets-context".into(),
        team: "riverside".into(),
    });
    for (verb, args, expected) in [
        (
            "routeToContext",
            json!({"logicalName":"story.md"}),
            "story.md",
        ),
        ("draft", json!({"from":"source.md"}), "source.md"),
        ("getWorkspaceStatus", json!({"storyId":"S-1"}), "S-1"),
        ("setHumanStatus", json!({"status":"away"}), "away"),
        ("archive", json!({"messageIds":[1,2,3]}), "3 messages"),
        ("markRead", json!({"messageIds":[1]}), "1 messages"),
        (
            "setStatus",
            json!({"path":["a.md","b.md"],"status":"done"}),
            "2 docs",
        ),
        (
            "setStatus",
            json!({"path":"story.md","status":"done"}),
            "story.md",
        ),
        (
            "setField",
            json!({"path":"story.md","field":"status","value":"done"}),
            "story.md (status)",
        ),
        (
            "setField",
            json!({"path":"story.md","fields":{"priority":"p1","status":"done"}}),
            "story.md (priority, status)",
        ),
        (
            "openFile",
            json!({"path":"story.md","team":"backend"}),
            "backend · story.md",
        ),
        ("openWorkspace", json!({"storyId":"S-1"}), "S-1"),
        (
            "openWorkspace",
            json!({"target":{"kind":"story","storyId":"S-2"}}),
            "S-2",
        ),
        (
            "openWorkspace",
            json!({"target":{"kind":"branch","repo":"acme/app","branch":"feature"}}),
            "acme/app#feature",
        ),
        (
            "openWorkspace",
            json!({"target":{"kind":"dir","path":" /work/project "}}),
            "/work/project",
        ),
        (
            "openWorkspace",
            json!({"target":{"kind":"epic","team":"acme/widgets-context:riverside","epic":"epics/test"}}),
            "acme/widgets-context:riverside · epics/test",
        ),
        (
            "openWorkspace",
            json!({"target":{"kind":"epic","team":"acme/widgets-context:backend","epic":"epics/test"}}),
            "acme/widgets-context:backend · epics/test (another team)",
        ),
        ("openTab", json!({"workspaceId":"workspace"}), "workspace"),
        (
            "sendToWorkspaceTab",
            json!({"sessionId":"tab","mode":"prompt","text":"private payload"}),
            "prompt → tab",
        ),
        (
            "runPrompt",
            json!({"target":"session","prompt":"private payload"}),
            "session",
        ),
        (
            "sendToSession",
            json!({"target":"session","text":"private payload"}),
            "session",
        ),
        (
            "runSkill",
            json!({"skill":"review","args":["private payload"]}),
            "review",
        ),
        ("readWorkspace", json!({"path":"./story.md"}), "./story.md"),
        ("closeWorkspace", json!({"target":"workspace"}), "workspace"),
        ("stopSession", json!({"handle":"session"}), "session"),
        (
            "sendPrompt",
            json!({"handle":"agent#1", "text":"private payload"}),
            "agent#1",
        ),
        ("closeTerminalTab", json!({"handle":"tab"}), "tab"),
        (
            "sendAsHuman",
            json!({"to":"recipient","text":"private payload"}),
            "recipient",
        ),
        (
            "replyAsHuman",
            json!({"messageId":42,"text":"private payload"}),
            "message 42",
        ),
        ("openMessage", json!({"messageId":42}), "message 42"),
    ] {
        assert_eq!(
            super::dispatcher::action_target(&caller, verb, &args).as_deref(),
            Some(expected),
            "{verb}"
        );
    }
}

// Exercise a real capability mint/resolve followed by native admission. The
// record's caller kind is deliberately independent of its capability scope.
fn minted_frame_caller(message: bool, scope: &str) -> Caller {
    use crate::dashboard_stream::frame::{
        FrameGrant, FrameMessage, FramePolicy, FrameStore, FrameTeam,
    };
    let store = FrameStore::default();
    let record = store.mint(
        9321,
        FrameGrant {
            frame_id: String::new(),
            principal: if message { "reviewer" } else { "human" }.into(),
            dashboard: if message {
                "inbox-message://acme/team/7"
            } else {
                "/ctx/team/board.html"
            }
            .into(),
            slot: if message { "message" } else { "centre" }.into(),
            team: Some(FrameTeam {
                org: "acme".into(),
                team: "team".into(),
            }),
            scope: scope.into(),
        },
        "/ctx/team".into(),
        "/ctx/team/board.html".into(),
        "/ctx/team".into(),
        FramePolicy::default(),
        message.then(|| FrameMessage {
            org: "acme".into(),
            team: "team".into(),
            message_id: 7,
        }),
    );
    let grant = store.resolve(&record.origin, &record.token).unwrap();
    let mut caller = serde_json::to_value(&grant).unwrap();
    assert_eq!(
        caller.as_object().unwrap().len(),
        6,
        "FrameGrant remains six fields"
    );
    caller["kind"] = serde_json::to_value(&record).unwrap()["kind"].clone();
    serde_json::from_value(caller).unwrap()
}

#[test]
fn minted_message_kind_obeys_native_team_safe_with_open_or_restricted_capabilities() {
    let mut policy = Policy::default();
    policy.preset = Preset::TeamSafe;
    for scope in ["open", "team"] {
        for verb in ["sendAsHuman", "stopSession", "sendToSession"] {
            let caller = minted_frame_caller(true, scope);
            assert_eq!(caller.principal, "reviewer");
            assert_eq!(
                Dispatcher::default().dispatch(caller, verb, json!({}), &policy, 0),
                Decision::Deny {
                    reason: "denied: message frame".into()
                },
                "{scope} {verb}"
            );
        }
    }
}

#[test]
fn minted_team_kind_obeys_native_confirm_toggle_with_open_capabilities() {
    let mut policy = Policy::default();
    policy.switches.confirm_team_send_as_you = true;
    for verb in ["sendAsHuman", "sendToSession"] {
        assert!(
            matches!(
                Dispatcher::default().dispatch(
                    minted_frame_caller(false, "open"),
                    verb,
                    json!({}),
                    &policy,
                    0
                ),
                Decision::Pending {
                    allow_always: false,
                    ..
                }
            ),
            "{verb}"
        );
    }
}

#[test]
fn minted_open_capabilities_preserve_default_native_admission_for_every_caller_kind() {
    for message in [true, false] {
        for verb in ["sendAsHuman", "stopSession", "sendToSession"] {
            assert!(
                matches!(
                    Dispatcher::default().dispatch(
                        minted_frame_caller(message, "open"),
                        verb,
                        json!({}),
                        &Policy::default(),
                        0
                    ),
                    Decision::Admit { .. }
                ),
                "{message} {verb}"
            );
        }
    }
}

#[test]
fn inbox_paging_is_admitted_as_a_read_and_subscribe_describes_windows() {
    let registry = Registry::default();
    let page = registry
        .get("page")
        .expect("SDK cursor paging is registered");
    assert_eq!(page.class, Class::Read);
    assert!(!page.action && !page.confirmable && !page.audited);
    assert_eq!(page.args_schema["required"], json!(["topic", "cursor"]));
    for scope in ["private", "builtin", "team", "message"] {
        assert!(matches!(
            Dispatcher::default().dispatch(
                caller(scope),
                "page",
                json!({"topic":"inbox","cursor":"older"}),
                &Policy::default(),
                0
            ),
            Decision::Admit { .. }
        ));
    }
    let schema = &registry.get("subscribe").unwrap().args_schema;
    assert_eq!(schema["properties"]["window"]["minItems"], 2);
    assert_eq!(schema["properties"]["window"]["maxItems"], 2);
}

#[test]
fn flush_writes_skip_rate_budgets_but_return_to_normal_afterwards() {
    let mut dispatcher = Dispatcher::default();
    let policy = Policy::default();
    for _ in 0..30 {
        assert!(matches!(
            dispatcher.dispatch(caller("private"), "setPrefs", json!({}), &policy, 0),
            Decision::Admit { .. }
        ));
    }
    assert!(matches!(
        dispatcher.dispatch(caller("private"), "setPrefs", json!({}), &policy, 0),
        Decision::Deny { .. }
    ));
    dispatcher.set_flushing("frame", true);
    assert!(matches!(
        dispatcher.dispatch(caller("private"), "setPrefs", json!({}), &policy, 0),
        Decision::Admit { .. }
    ));
    dispatcher.set_flushing("frame", false);
    assert!(matches!(
        dispatcher.dispatch(caller("private"), "setPrefs", json!({}), &policy, 0),
        Decision::Deny { .. }
    ));
}

#[test]
fn frame_presentation_verbs_have_no_mcp_twin_and_use_native_admission() {
    let registry = Registry::default();
    let contract = crate::mcp::live_data_authoring::local_contract_payload();
    for name in ["showMenu", "showPopover", "focusOut", "focusTerminal"] {
        let spec = registry
            .get(name)
            .expect("registered frame presentation verb");
        assert!(spec.presentation_only_reason.is_some());
        assert!(spec.mcp_twin.is_none());
        assert_eq!(spec.grant, "sessions:read");
        let row = contract["sdk"]["verbs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["verb"] == name)
            .unwrap();
        assert!(row["presentationOnlyReason"].is_string());
        assert!(row["mcpTwin"].is_null());
        let mut dispatcher = Dispatcher::default();
        assert!(matches!(
            dispatcher.dispatch(caller("team"), name, json!({}), &Policy::default(), 0),
            Decision::Admit { .. }
        ));
    }
}
#[test]
fn commands_register_is_a_frame_scoped_presentation_verb() {
    let registry = Registry::default();
    let spec = registry.get("commands.register").unwrap();
    assert_eq!(spec.module, "commands");
    assert_eq!(spec.class, Class::Read);
    assert!(!spec.action && !spec.confirmable && spec.mcp_twin.is_none());
    assert!(spec
        .presentation_only_reason
        .as_deref()
        .unwrap()
        .starts_with("Frame-scoped declaration"));
    assert!(jsonschema::is_valid(
        &spec.args_schema,
        &json!({"ids":["archive"]})
    ));
    assert!(!jsonschema::is_valid(&spec.args_schema, &json!({})));
}

#[test]
fn menu_target_schema() {
    let registry = Registry::default();
    let schema = &registry.get("showMenu").unwrap().args_schema;
    let anchor = json!({"x":2,"y":3,"width":0,"height":0});
    for target in ["row", "text", "frame"] {
        assert!(jsonschema::is_valid(
            schema,
            &json!({"anchorRect":anchor,"target":target,"text":{"editable":true,"hasSelection":false,"hasText":true,"password":false}})
        ));
    }
    assert!(!jsonschema::is_valid(
        schema,
        &json!({"anchorRect":anchor,"target":"unknown"})
    ));
    assert!(!jsonschema::is_valid(
        schema,
        &json!({"anchorRect":anchor,"text":{"editable":"yes"}})
    ));
}

#[test]
fn mark_unread_has_the_metadata_contract_and_mcp_twin() {
    let registry = Registry::default();
    let verb = registry.get("markUnread").unwrap();
    assert_eq!(verb.class, Class::Metadata);
    assert_eq!(verb.mcp_twin.as_deref(), Some("mark_unread"));
    assert!(verb.inbox_target);
    assert!(!verb.confirmable);
    assert_eq!(verb.args_schema["required"], json!(["messageIds"]));
    assert_eq!(verb.result_schema["required"], json!(["marked"]));
}

#[test]
fn media_load_is_a_team_bound_inbox_write_without_a_config_write_verb() {
    let registry = Registry::default();
    let verb = registry.get("loadMedia").unwrap();
    assert_eq!(verb.grant, "inbox:write");
    assert!(verb.action && verb.inbox_target);
    assert!(jsonschema::is_valid(
        &verb.args_schema,
        &json!({"messageId":1})
    ));
    assert!(!jsonschema::is_valid(&verb.args_schema, &json!({})));
    assert!(registry.get("setMediaConfig").is_none());
}

fn review_media_frame(
    kind: crate::dashboard_stream::frame::FrameCallerKind,
    inbox: bool,
) -> crate::dashboard_stream::frame::FrameRecord {
    use crate::dashboard_stream::frame::*;
    FrameStore::default().mint_with_grant(
        9321,
        FrameGrant {
            frame_id: String::new(),
            principal: "42".into(),
            dashboard: "inbox".into(),
            slot: "right-pane".into(),
            team: None,
            scope: "open".into(),
        },
        "/ctx".into(),
        "/ctx/inbox.html".into(),
        "/ctx".into(),
        FramePolicy {
            inbox_media: inbox,
            ..Default::default()
        },
        None,
        vec!["inbox:write".into()],
        kind,
    )
}
#[test]
fn review_01_message_frames_cannot_load_media() {
    let frame = review_media_frame(
        crate::dashboard_stream::frame::FrameCallerKind::Message,
        false,
    );
    assert!(matches!(
        Dispatcher::default().dispatch_frame(
            &frame,
            "loadMedia",
            json!({"messageId":1}),
            &Policy::default(),
            0
        ),
        Decision::Deny { .. }
    ));
}
#[test]
fn review_01_inbox_media_always_requires_host_confirmation() {
    let frame = review_media_frame(
        crate::dashboard_stream::frame::FrameCallerKind::Builtin,
        true,
    );
    let mut dispatcher = Dispatcher::default();
    let decision = dispatcher.dispatch_frame(
        &frame,
        "loadMedia",
        json!({"messageId":1}),
        &Policy::default(),
        0,
    );
    let Decision::Pending {
        confirm_id,
        allow_always,
    } = decision
    else {
        panic!("a real host confirmation is required: {decision:?}")
    };
    assert!(!allow_always);
    assert_eq!(
        dispatcher.answer(&confirm_id, true).unwrap().verb,
        "loadMedia"
    );
}

#[test]
fn open_diff_uses_existing_repo_read_and_typed_after_range_contract() {
    let registry = Registry::default();
    let spec = registry.get("openDiff").unwrap();
    assert_eq!(spec.grant, "repo:read");
    assert!(spec.audited && spec.action && !spec.confirmable);
    assert!(jsonschema::is_valid(&spec.args_schema, &json!({"repo":"acme/api","rev":"abcdef1","path":"a.rs","range":{"start":1,"end":2,"side":"after"}})));
    assert!(!jsonschema::is_valid(&spec.args_schema, &json!({"repo":"acme/api","rev":"abcdef1","path":"a.rs","range":{"start":1,"end":2,"side":"before"}})));
    for value in [json!({"status":"opened"}),json!({"status":"commitNotLocal","reason":"not local","fetchCommand":"git fetch origin abcdef1"}),json!({"status":"worktreeRemoved","reason":"removed"}),json!({"status":"refused","reason":"old side"}),json!({"status":"unavailable","reason":"not attached"})] {
        assert!(jsonschema::is_valid(&spec.result_schema, &value));
    }
}
