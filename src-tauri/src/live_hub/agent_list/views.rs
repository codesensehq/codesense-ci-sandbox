// SPDX-License-Identifier: MIT
use super::AgentListRow;
use crate::live_hub::projection::{Delta, TopicRegistry};
use serde_json::Value;

pub(crate) fn replace(
    registry: &mut TopicRegistry,
    key: &str,
    rows: &[AgentListRow],
    meta: &Value,
) -> Result<Vec<Delta>, String> {
    let mut rows: Vec<_> = rows
        .iter()
        .filter(|row| {
            super::super::view_key::admits(
                key,
                Some(&format!("team:{}", row.team_key.replacen(':', "/", 1))),
            )
        })
        .map(|r| serde_json::to_value(r).unwrap())
        .collect();
    let previous = registry.snapshot("agentList", key)?.snapshot;
    super::order::positions(&mut rows, &previous);
    let mut meta = meta.clone();
    if key != "all" {
        let admitted = |team: &str| {
            super::super::view_key::admits(
                key,
                Some(&format!("team:{}", team.replacen(':', "/", 1))),
            )
        };
        for field in ["teamNames", "workspaceRollups"] {
            if let Some(teams) = meta.get_mut(field).and_then(Value::as_object_mut) {
                teams.retain(|team, _| admitted(team));
            }
        }
        if let Some(pane) = meta.get_mut("pane").and_then(Value::as_object_mut) {
            for field in ["rails", "railKeys", "workspaceOrder"] {
                if let Some(teams) = pane.get_mut(field).and_then(Value::as_object_mut) {
                    teams.retain(|team, _| admitted(team));
                }
            }
            if !pane
                .get("workspaceTeam")
                .and_then(Value::as_str)
                .is_some_and(admitted)
            {
                for field in ["workspaceTeam", "workspaceLabel", "workspaceWait"] {
                    pane.remove(field);
                }
            }
            if let Some(ids) = pane.get_mut("pinnedRefs").and_then(Value::as_array_mut) {
                ids.retain(|id| rows.iter().any(|row| row["id"] == *id));
            }
            for field in ["openTeams", "teamOrder"] {
                if let Some(teams) = pane.get_mut(field).and_then(Value::as_array_mut) {
                    teams.retain(|team| team.as_str().is_some_and(admitted));
                }
            }
        }
        if let Some(teams) = meta["teamOrder"].as_array_mut() {
            teams.retain(|team| team.as_str().is_some_and(admitted));
        }
        if !meta["homeTeam"].as_str().is_some_and(admitted) {
            meta["homeTeam"] = Value::Null;
        }
    }
    if let Some(spend) = meta.as_object_mut().and_then(|m| m.remove("spendRows")) {
        rows.extend(
            spend
                .as_array()
                .into_iter()
                .flatten()
                .filter(|row| {
                    super::super::view_key::admits(
                        key,
                        Some(&format!(
                            "team:{}",
                            row["teamKey"]
                                .as_str()
                                .unwrap_or_default()
                                .replacen(':', "/", 1)
                        )),
                    )
                })
                .cloned(),
        );
    }
    meta["id"] = "agentList:state".into();
    meta["rowType"] = "state".into();
    meta["order"] = f64::MAX.into();
    rows.push(meta);
    registry.replace_view(
        "agentList",
        key,
        rows.into_iter()
            .map(|row| {
                (
                    Some(if row["rowType"] == "state" {
                        key.strip_suffix("|status").unwrap_or(key).to_string()
                    } else {
                        format!(
                            "team:{}",
                            row["teamKey"]
                                .as_str()
                                .unwrap_or_default()
                                .replacen(':', "/", 1)
                        )
                    }),
                    row,
                )
            })
            .collect(),
    )
}

/// A distinct view consumes session status and inbox counts without copying either field.
pub(crate) fn replace_status(
    registry: &mut TopicRegistry,
    key: &str,
    rows: &[AgentListRow],
    meta: &Value,
    sessions: &[Value],
    unread: &std::collections::BTreeMap<String, u32>,
) -> Result<Vec<Delta>, String> {
    let mut ordered = rows.to_vec();
    let session =
        |reference: &AgentListRow| sessions.iter().find(|s| s["id"] == reference.session_ref);
    let rank = |reference: &AgentListRow| {
        session(reference)
            .and_then(|s| {
                serde_json::from_value::<crate::live_hub::effective_status::AgentStatus>(
                    s["effectiveStatus"].clone(),
                )
                .ok()
            })
            .map_or(3, |status| status.rank())
    };
    let handle = |reference: &AgentListRow| {
        session(reference)
            .and_then(|s| s["handle"].as_str())
            .unwrap_or_default()
    };
    ordered.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| {
                unread
                    .get(handle(b))
                    .copied()
                    .unwrap_or(0)
                    .cmp(&unread.get(handle(a)).copied().unwrap_or(0))
            })
            .then_with(|| compare_handles(handle(a), handle(b)))
    });
    for row in &mut ordered {
        row.status_group = Some(rank(row));
    }
    replace(registry, key, &ordered, meta)
}

fn compare_handles(left: &str, right: &str) -> std::cmp::Ordering {
    // Root collation for handle characters: punctuation, digits, case-folded letters;
    // case only breaks a complete primary tie, with lowercase before uppercase.
    let primary = |byte: u8| match byte {
        b' ' => (0, 0),
        b'_' => (0, 1),
        b'-' => (0, 2),
        b'.' => (0, 3),
        b'/' => (0, 4),
        b'#' => (0, 5),
        b'0'..=b'9' => (1, byte),
        b'a'..=b'z' | b'A'..=b'Z' => (2, byte.to_ascii_lowercase()),
        _ => (3, byte),
    };
    left.bytes()
        .map(primary)
        .cmp(right.bytes().map(primary))
        .then_with(|| {
            left.bytes()
                .map(|b| b.is_ascii_uppercase())
                .cmp(right.bytes().map(|b| b.is_ascii_uppercase()))
        })
        .then_with(|| left.cmp(right))
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn scoped_views_preserve_absent_and_null_metadata() {
        let mut registry = TopicRegistry::default();
        for meta in [
            json!({"ready":true}),
            json!({"ready":false,"teamNames":null,"workspaceRollups":null,"pane":null}),
            json!({"ready":true,"teamNames":{},"workspaceRollups":{},"pane":{}}),
        ] {
            for key in ["all", "all|status", "team:acme/a", "team:acme/a|status"] {
                replace(&mut registry, key, &[], &meta).unwrap();
                let view = registry.snapshot("agentList", key).unwrap();
                let state = view
                    .snapshot
                    .iter()
                    .find(|row| row["rowType"] == "state")
                    .unwrap();
                for field in ["teamNames", "workspaceRollups", "pane"] {
                    assert_eq!(state.get(field), meta.get(field), "{key}: {field}");
                }
            }
        }
    }
    #[test]
    fn stored_grant_status_view_scopes_rows_and_metadata() {
        use crate::dashboard_stream::frame::{FrameGrant, FrameTeam};
        let sessions = vec![
            json!({"id":"idle-a","handle":"z","effectiveStatus":"idle"}),
            json!({"id":"needs-b","handle":"a","effectiveStatus":"awaiting-answer"}),
            json!({"id":"working-a","handle":"b","effectiveStatus":"running"}),
            json!({"id":"needs-a","handle":"c","effectiveStatus":"awaiting-answer"}),
        ];
        let rows: Vec<AgentListRow> = sessions.iter().enumerate().map(|(i,s)| serde_json::from_value(json!({
            "id":s["id"],"sessionRef":s["id"],"inboxRef":"counts","teamKey":if i==1 {"acme:b"} else {"acme:a"},
            "placementTeamRef":null,"parentId":null,"parentSource":null,"order":0,
            "workspaceRef":null,"storyRef":null,"workspaceRank":0,"crewParentId":null,"crewOrder":null,"crewLead":false
        })).unwrap()).collect();
        let meta = json!({"ready":true,"homeTeam":"acme:b","teamOrder":["acme:a","acme:b"],
            "teamNames":{"acme:a":{"team":"a"},"acme:b":{"team":"b"}},
            "workspaceRollups":{"acme:a":{},"acme:b":{}},
            "pane":{"openTeams":["acme:a","acme:b"],"teamOrder":["acme:b","acme:a"],
                "rails":{"acme:a":[],"acme:b":[]},"railKeys":{"acme:a":{},"acme:b":{}},
                "workspaceTeam":"acme:b","workspaceLabel":"Other","workspaceWait":true,
                "pinnedRefs":["idle-a","needs-b"]}});
        let mut registry = TopicRegistry::default();
        for scope in ["team", "open"] {
            let grant = FrameGrant {
                frame_id: "test".into(),
                principal: "human".into(),
                dashboard: "pane".into(),
                slot: "dock".into(),
                scope: scope.into(),
                team: Some(FrameTeam {
                    org: "acme".into(),
                    team: "a".into(),
                }),
            };
            let base = crate::live_hub::view_key::resolve(&grant).unwrap();
            let key = format!("{base}|status");
            replace_status(
                &mut registry,
                &key,
                &rows,
                &meta,
                &sessions,
                &Default::default(),
            )
            .unwrap();
            let view = registry.snapshot("agentList", &key).unwrap();
            let agents: Vec<_> = view
                .snapshot
                .iter()
                .filter(|r| r["rowType"] != "state")
                .collect();
            let expected = if scope == "team" {
                json!(["needs-a", "working-a", "idle-a"])
            } else {
                json!(["needs-b", "needs-a", "working-a", "idle-a"])
            };
            assert_eq!(
                json!(agents.iter().map(|r| &r["id"]).collect::<Vec<_>>()),
                expected
            );
            assert_eq!(
                json!(agents.iter().map(|r| &r["statusGroup"]).collect::<Vec<_>>()),
                if scope == "team" {
                    json!([0, 1, 2])
                } else {
                    json!([0, 0, 1, 2])
                }
            );
            let state = view
                .snapshot
                .iter()
                .find(|r| r["rowType"] == "state")
                .unwrap();
            if scope == "team" {
                assert!(agents.iter().all(|r| r["teamKey"] == "acme:a"));
                assert_eq!(state["teamOrder"], json!(["acme:a"]));
                assert!(state["homeTeam"].is_null());
                for field in ["teamNames", "workspaceRollups"] {
                    assert!(state[field].get("acme:b").is_none());
                }
                for field in ["rails", "railKeys"] {
                    assert!(state["pane"][field].get("acme:b").is_none());
                }
                assert_eq!(state["pane"]["openTeams"], json!(["acme:a"]));
                assert_eq!(state["pane"]["pinnedRefs"], json!(["idle-a"]));
                assert!(state["pane"].get("workspaceTeam").is_none());
            } else {
                assert_eq!(state["teamOrder"], meta["teamOrder"]);
                assert_eq!(state["teamNames"], meta["teamNames"]);
            }
        }
    }
    #[test]
    fn shared_status_order_and_unread_only_move_status_subscribers() {
        let fixture: Value =
            serde_json::from_str(include_str!("../effective_status/parity.json")).unwrap();
        let sessions = fixture["order"]["rows"].as_array().unwrap();
        let rows: Vec<AgentListRow> = sessions.iter().map(|s| serde_json::from_value(json!({
            "id":s["id"],"sessionRef":s["id"],"inboxRef":"counts:team:o/a","teamKey":"o:a",
            "placementTeamRef":null,"parentId":null,"parentSource":null,"order":0,
            "workspaceRef":null,"storyRef":null,"workspaceRank":0,"crewParentId":null,"crewOrder":null,"crewLead":false
        })).unwrap()).collect();
        let mut unread = sessions
            .iter()
            .map(|s| {
                (
                    s["handle"].as_str().unwrap().to_string(),
                    s["unread"].as_u64().unwrap() as u32,
                )
            })
            .collect();
        let meta = json!({"ready":true,"homeTeam":"o:a","teamOrder":["o:a"]});
        let mut registry = TopicRegistry::default();
        replace(&mut registry, "all", &rows, &meta).unwrap();
        replace_status(&mut registry, "all|status", &rows, &meta, sessions, &unread).unwrap();
        replace_status(
            &mut registry,
            "team:o/a|status",
            &rows,
            &meta,
            sessions,
            &unread,
        )
        .unwrap();
        let stable_rev = registry.snapshot("agentList", "all").unwrap().rev;
        let status = registry.snapshot("agentList", "all|status").unwrap();
        let ordered: Vec<_> = status
            .snapshot
            .iter()
            .filter(|row| row["rowType"] != "state")
            .map(|row| row["id"].clone())
            .collect();
        assert_eq!(json!(ordered), fixture["order"]["expected"]);
        assert_eq!(
            registry
                .snapshot("agentList", "team:o/a|status")
                .unwrap()
                .snapshot,
            status.snapshot
        );
        unread.insert("b".into(), 10);
        let deltas =
            replace_status(&mut registry, "all|status", &rows, &meta, sessions, &unread).unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].upserts.len(), 1);
        assert!(serde_json::to_vec(&deltas[0]).unwrap().len() < 2048);
        assert_eq!(
            registry.snapshot("agentList", "all").unwrap().rev,
            stable_rev
        );
        assert_eq!(
            registry
                .snapshot("agentList", "team:o/a|status")
                .unwrap()
                .rev,
            status.rev
        );
        let mut changed = sessions.clone();
        changed.iter_mut().find(|row| row["id"] == "z").unwrap()["effectiveStatus"] =
            "paused".into();
        assert!(
            !replace_status(&mut registry, "all|status", &rows, &meta, &changed, &unread)
                .unwrap()
                .is_empty()
        );
        assert!(replace(&mut registry, "all", &rows, &meta)
            .unwrap()
            .is_empty());
        assert_eq!(
            registry.snapshot("agentList", "all").unwrap().rev,
            stable_rev
        );
    }
}

#[cfg(test)]
mod workspace_order_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn workspace_orders_follow_team_scope_in_plain_and_status_views() {
        let meta = json!({"pane":{"workspaceOrder":{"o:a":["two","one"],"o:b":["secret"]}}});
        let mut registry = TopicRegistry::default();
        for suffix in ["", "|status"] {
            for scope in ["all", "team:o/a"] {
                let key = format!("{scope}{suffix}");
                replace(&mut registry, &key, &[], &meta).unwrap();
                let state = registry
                    .snapshot("agentList", &key)
                    .unwrap()
                    .snapshot
                    .pop()
                    .unwrap();
                assert_eq!(
                    state["pane"]["workspaceOrder"]["o:a"],
                    json!(["two", "one"])
                );
                assert_eq!(
                    state["pane"]["workspaceOrder"].get("o:b").is_some(),
                    scope == "all"
                );
            }
        }
    }
}
