// SPDX-License-Identifier: MIT
use crate::dashboard_stream::frame::FrameGrant;

/// Shared by first paint and live subscriptions, using the stored frame grant.
pub fn resolve(grant: &FrameGrant) -> Option<String> {
    if grant.scope == "team" || (grant.slot == "file-nav" && grant.team.is_some()) {
        grant
            .team
            .as_ref()
            .map(|team| team_key(&team.org, &team.team))
    } else {
        Some("all".into())
    }
}

pub fn team_key(org: &str, team: &str) -> String {
    format!("team:{org}/{team}")
}

pub fn admits(key: &str, scope: Option<&str>) -> bool {
    let key = key.split("|").next().unwrap_or(key);
    key == "all" || scope == Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard_stream::frame::{FrameMessage, FramePolicy, FrameStore, FrameTeam};
    use crate::live_hub::projection::TopicRegistry;
    use serde_json::json;

    #[test]
    fn stored_grant_scopes_sessions_inbox_messages_and_count_rows_together() {
        let mut topics = TopicRegistry::default();
        let mut sessions = Vec::new();
        let mut inbox = Vec::new();
        for (team, unread) in [("a", 1), ("b", 7)] {
            let key = Some(team_key("acme", team));
            sessions.push((key.clone(), json!({"id":team,"handle":team})));
            inbox.push((
                key.clone(),
                json!({"id":format!("counts:{team}"),"rowType":"counts","humanUnread":unread}),
            ));
            inbox.push((
                key,
                json!({"id":format!("message:{team}"),"rowType":"message","text":team}),
            ));
        }
        topics.replace("sessions", sessions, None).unwrap();
        topics.replace("inbox", inbox, None).unwrap();
        for (scope, slot) in [
            ("open", "centre"),
            ("private", "centre"),
            ("team", "centre"),
            ("open", "message"),
            ("team", "message"),
        ] {
            let store = FrameStore::default();
            let record = store.mint(
                9321,
                FrameGrant {
                    frame_id: String::new(),
                    principal: "human".into(),
                    dashboard: "/ctx/acme/a/inbox.html".into(),
                    slot: slot.into(),
                    team: (scope != "private").then(|| FrameTeam {
                        org: "acme".into(),
                        team: "a".into(),
                    }),
                    scope: scope.into(),
                },
                "/ctx/acme/a".into(),
                "/ctx/acme/a/inbox.html".into(),
                "/ctx/acme/a".into(),
                FramePolicy::default(),
                (slot == "message").then(|| FrameMessage {
                    org: "acme".into(),
                    team: "a".into(),
                    message_id: 1,
                }),
            );
            let grant = store.resolve(&record.origin, &record.token).unwrap();
            let key = resolve(&grant).unwrap();
            let sessions = topics.snapshot("sessions", &key).unwrap();
            let inbox = topics.snapshot("inbox", &key).unwrap();
            let counts: u64 = inbox
                .snapshot
                .iter()
                .filter_map(|row| row["humanUnread"].as_u64())
                .sum();
            if scope == "team" {
                assert_eq!(key, "team:acme/a");
                assert_eq!(sessions.snapshot, vec![json!({"id":"a","handle":"a"})]);
                assert_eq!(inbox.snapshot.len(), 2);
                assert!(inbox
                    .snapshot
                    .iter()
                    .all(|row| row["id"].as_str().unwrap().ends_with(":a")));
                assert_eq!(counts, 1);
            } else {
                assert_eq!(key, "all");
                assert_eq!(sessions.snapshot.len(), 2);
                assert_eq!(inbox.snapshot.len(), 4);
                assert_eq!(counts, 8);
            }
            let window = topics
                .subscribe_window("grant-window", "inbox", &key, None, Some([0, 1]))
                .unwrap();
            assert_eq!(window.total, Some(inbox.snapshot.len()));
            assert_eq!(window.snapshot.unwrap(), inbox.snapshot[..1]);
        }
    }
}
