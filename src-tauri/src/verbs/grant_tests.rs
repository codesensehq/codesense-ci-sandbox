// SPDX-License-Identifier: MIT
use super::*;
use crate::dashboard_stream::frame::{FrameCallerKind, FrameGrant, FramePolicy, FrameStore};
use serde_json::json;

#[test]
fn all_registered_verbs_name_a_grantable_capability() {
    for verb in builtin_registry().all() {
        assert!(
            capabilities::known(&verb.grant),
            "{} names unknown capability {:?}",
            verb.name,
            verb.grant
        );
    }
}

#[test]
fn projection_admission_uses_the_topic_and_resolved_frame_view() {
    let store = FrameStore::default();
    let mut record = store.mint(
        9321,
        FrameGrant {
            frame_id: String::new(),
            principal: "human".into(),
            dashboard: "board".into(),
            slot: "right-pane".into(),
            team: Some(crate::dashboard_stream::frame::FrameTeam {
                org: "acme".into(),
                team: "a".into(),
            }),
            scope: "team".into(),
        },
        "/ctx".into(),
        "/ctx/board.html".into(),
        "/ctx".into(),
        FramePolicy::default(),
        None,
    );
    record.effective_grant = vec!["sessions:read".into(), "inbox:read:team".into()];
    for verb in ["subscribe", "unsubscribe", "page"] {
        assert!(matches!(
            Dispatcher::default().dispatch_frame(
                &record,
                verb,
                json!({"topic":"sessions"}),
                &Policy::default(),
                0
            ),
            Decision::Admit { .. }
        ));
        assert!(matches!(
            Dispatcher::default().dispatch_frame(
                &record,
                verb,
                json!({"topic":"inbox"}),
                &Policy::default(),
                0
            ),
            Decision::Admit { .. }
        ));
        record.grant.scope = "open".into();
        assert_eq!(
            Dispatcher::default().dispatch_frame(
                &record,
                verb,
                json!({"topic":"inbox"}),
                &Policy::default(),
                0
            ),
            Decision::Deny {
                reason: "denied: grant".into()
            }
        );
        record.grant.scope = "team".into();
    }
}

#[test]
fn agent_list_actions_use_only_their_existing_frame_capability() {
    let store = FrameStore::default();
    let mut frame = store.mint(
        9321,
        FrameGrant {
            frame_id: String::new(),
            principal: "human".into(),
            dashboard: "agents".into(),
            slot: "dock".into(),
            team: None,
            scope: "team".into(),
        },
        "/ctx".into(),
        "/ctx/agents.html".into(),
        "/ctx".into(),
        FramePolicy::default(),
        None,
    );
    for (verb, capability) in [
        ("moveWorkspace", "sessions:read"),
        ("launchUnder", "sessions:read"),
        ("openAgentInbox", "inbox:read:team"),
    ] {
        frame.effective_grant = vec![capability.into()];
        assert!(matches!(
            Dispatcher::default().dispatch_frame(&frame, verb, json!({}), &Policy::default(), 0),
            Decision::Admit { .. }
        ));
        frame.effective_grant.clear();
        assert_eq!(
            Dispatcher::default().dispatch_frame(&frame, verb, json!({}), &Policy::default(), 0),
            Decision::Deny {
                reason: "denied: grant".into()
            }
        );
    }
}

#[test]
fn open_frame_admits_registered_verbs_except_inbox_media_and_team_safe_denies_team_inbox() {
    let store = FrameStore::default();
    let mut record = store.mint(
        9321,
        FrameGrant {
            frame_id: String::new(),
            principal: "human".into(),
            dashboard: "board".into(),
            slot: "right-pane".into(),
            team: None,
            scope: "open".into(),
        },
        "/ctx".into(),
        "/ctx/board.html".into(),
        "/ctx".into(),
        FramePolicy::default(),
        None,
    );
    for verb in builtin_registry().all() {
        if verb.name == "loadMedia" {
            assert!(matches!(
                Dispatcher::default().dispatch_frame(
                    &record,
                    &verb.name,
                    json!({}),
                    &Policy::default(),
                    0
                ),
                Decision::Deny { .. }
            ));
            continue;
        }
        assert!(
            matches!(
                Dispatcher::default().dispatch_frame(
                    &record,
                    &verb.name,
                    json!({}),
                    &Policy::default(),
                    0
                ),
                Decision::Admit { .. }
            ),
            "{}",
            verb.name
        );
    }
    let policy = Policy {
        preset: Preset::TeamSafe,
        ..Policy::default()
    };
    record.effective_grant = policy.effective_grant(
        "board",
        "right-pane",
        "team",
        &capabilities::full(),
        &capabilities::full(),
    );
    assert_eq!(
        Dispatcher::default().dispatch_frame(&record, "inboxRollup", json!({}), &policy, 0),
        Decision::Deny {
            reason: "denied: grant".into()
        }
    );
}

#[test]
fn effective_open_is_full_intersected_only_with_the_principal() {
    let policy = Policy::default();
    assert_eq!(
        policy.effective_grant("board", "dock", "team", &[], &capabilities::full()),
        capabilities::full()
    );
    assert_eq!(
        policy.effective_grant(
            "board",
            "right-pane",
            "message",
            &[],
            &["sessions:read".into()]
        ),
        vec!["sessions:read"]
    );
}

#[test]
fn effective_team_safe_and_explicit_overrides_use_the_same_vocabulary() {
    let mut policy: Policy = serde_json::from_value(json!({"preset":"team-safe","perDashboard":{"board":{"grant":["inbox:read:all-teams","sessions:read"]}}})).unwrap();
    let all = capabilities::full();
    let declared = all.clone();
    let team = policy.effective_grant("other", "right-pane", "team", &declared, &all);
    assert!(!team.contains(&"records:read:own".into()));
    assert!(!team.contains(&"inbox:read:all-teams".into()));
    assert!(team.contains(&"launch".into()));
    let dock = policy.effective_grant("other", "dock", "private", &declared, &all);
    assert!(!dock.contains(&"inbox:read:all-teams".into()));
    let widened = policy.effective_grant("board", "right-pane", "team", &[], &all);
    assert_eq!(widened, vec!["sessions:read", "inbox:read:all-teams"]);
    policy.preset = Preset::Open;
    assert_eq!(
        policy.effective_grant("board", "right-pane", "private", &declared, &all),
        widened
    );
    assert!(policy.effective_switches().private_install == false);
    policy.preset = Preset::TeamSafe;
    assert!(policy.effective_switches().private_install);
}

#[test]
fn stored_frame_grant_denies_before_budget_and_rejects_cross_team_reads() {
    let store = FrameStore::default();
    let mut record = store.mint(
        9321,
        FrameGrant {
            frame_id: String::new(),
            principal: "human".into(),
            dashboard: "board".into(),
            slot: "right-pane".into(),
            team: None,
            scope: "open".into(),
        },
        "/ctx".into(),
        "/ctx/board.html".into(),
        "/ctx".into(),
        FramePolicy::default(),
        None,
    );
    record.kind = FrameCallerKind::Team;
    record.effective_grant = vec![];
    let mut dispatcher = Dispatcher::default();
    dispatcher.set_flushing(&record.frame_id, true);
    for _ in 0..12 {
        for verb in ["sendToSession", "sendPrompt"] {
            assert_eq!(
                dispatcher.dispatch_frame(&record, verb, json!({}), &Policy::default(), 0),
                Decision::Deny {
                    reason: "denied: grant".into()
                }
            );
        }
    }
    assert!(dispatcher.windows.is_empty());
    record.effective_grant = vec!["sessions:send".into()];
    assert!(matches!(
        dispatcher.dispatch_frame(
            &record,
            "sendPrompt",
            json!({"handle":"agent", "text":"hello"}),
            &Policy::default(),
            0
        ),
        Decision::Admit { .. }
    ));
    record.grant.scope = "message".into();
    record.effective_grant = capabilities::full()
        .into_iter()
        .filter(|cap| capabilities::scope_allows("message", cap))
        .collect();
    assert_eq!(
        Dispatcher::default().dispatch_frame(
            &record,
            "sendPrompt",
            json!({}),
            &Policy::default(),
            0
        ),
        Decision::Deny {
            reason: "denied: grant".into()
        }
    );
    assert_eq!(
        dispatcher.dispatch_frame(&record, "inboxRollup", json!({}), &Policy::default(), 0),
        Decision::Deny {
            reason: "denied: grant".into()
        }
    );
}

#[test]
fn inherited_updates_and_human_status_require_all_team_capability_for_the_resolved_target() {
    let store = FrameStore::default();
    let mut frame = store.mint(
        9321,
        FrameGrant {
            frame_id: String::new(),
            principal: "human".into(),
            dashboard: "parent-board".into(),
            slot: "right-pane".into(),
            team: Some(crate::dashboard_stream::frame::FrameTeam {
                org: "acme".into(),
                team: "parent".into(),
            }),
            scope: "team".into(),
        },
        "/ctx/child/content".into(),
        "/ctx/parent/panes/board.html".into(),
        "/ctx/parent/panes".into(),
        FramePolicy::default(),
        None,
    );
    let policy = Policy {
        preset: Preset::TeamSafe,
        ..Default::default()
    };
    for verb in ["updates", "humanStatus"] {
        frame.effective_grant = vec!["inbox:read:team".into()];
        assert_eq!(
            Dispatcher::default().dispatch_frame(
                &frame,
                verb,
                json!({"org":"acme", "team":"child"}),
                &policy,
                0
            ),
            Decision::Deny {
                reason: "denied: grant".into()
            }
        );
        assert!(matches!(
            Dispatcher::default().dispatch_frame(&frame, verb, json!({}), &policy, 0),
            Decision::Admit { .. }
        ));
        frame.effective_grant.push("inbox:read:all-teams".into());
        assert!(matches!(
            Dispatcher::default().dispatch_frame(
                &frame,
                verb,
                json!({"org":"acme", "team":"child"}),
                &policy,
                0
            ),
            Decision::Admit { .. }
        ));
    }
}
