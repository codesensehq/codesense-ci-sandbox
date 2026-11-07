// SPDX-License-Identifier: MIT
use super::AgentListRow;
use serde_json::{json, Value};
use std::collections::BTreeMap;

// Mirrors shared/lib/status-mark.ts: mapping, availability, urgency and rollupWords.
fn mark(session: &Value) -> &'static str {
    let mark = match session["effectiveStatus"].as_str().unwrap_or("unknown") {
        "running" | "thinking" => "thinking",
        "tool-running" => "tool",
        "awaiting-answer" | "dropped" => "question",
        "paused" => "permission",
        "blocked" => "blocked",
        "failed" | "errored" => "error",
        "crashed" => "crashed",
        "stalled" => "stalled",
        "completed" | "done" => "ended",
        "idle" => "ready",
        _ => "unknown",
    };
    if session["state"] == "offline" && !matches!(mark, "ended" | "crashed") {
        "offline"
    } else {
        mark
    }
}

const MARKS: &[(&str, &str, &str)] = &[
    ("question", "needs you", "need you"),
    ("permission", "needs approval", "need approval"),
    ("blocked", "blocked", "blocked"),
    ("crashed", "crashed", "crashed"),
    ("error", "error", "errors"),
    ("tool", "running a tool", "running a tool"),
    ("thinking", "thinking", "thinking"),
    ("stalled", "stalled", "stalled"),
    ("ready", "ready", "ready"),
    ("unknown", "unknown", "unknown"),
    ("ended", "ended", "ended"),
    ("offline", "offline", "offline"),
];

fn urgency(mark: &str) -> u8 {
    match mark {
        "question" | "permission" | "blocked" => 5,
        "crashed" => 4,
        "error" => 3,
        "tool" | "thinking" => 2,
        "stalled" | "offline" => 1,
        _ => 0,
    }
}

/// Aggregate the session owners once on the host, in the native mark vocabulary.
pub(crate) fn workspace_rollups(
    rows: &[AgentListRow],
    sessions: &[(Option<String>, Value)],
) -> Value {
    let owners: BTreeMap<_, _> = sessions
        .iter()
        .map(|(_, s)| (s["id"].as_str().unwrap_or_default(), s))
        .collect();
    let mut groups = BTreeMap::<_, BTreeMap<&str, usize>>::new();
    for row in rows.iter() {
        if let Some(session) = owners.get(row.session_ref.as_str()) {
            *groups
                .entry((row.team_key.clone(), row.workspace_ref.clone()))
                .or_default()
                .entry(mark(session))
                .or_default() += 1;
        }
    }
    let mut rollups = json!({});
    for ((team, workspace), counts) in groups {
        if !rollups[&team].is_object() {
            rollups[&team] = json!({});
        }
        let present: Vec<_> = MARKS
            .iter()
            .filter(|(mark, _, _)| counts.contains_key(mark))
            .collect();
        let top = present
            .iter()
            .rev()
            .max_by_key(|(mark, _, _)| urgency(mark))
            .map_or("ready", |(mark, _, _)| *mark);
        let summary = present
            .iter()
            .map(|(mark, one, many)| {
                let n = counts[mark];
                format!("{n} {}", if n == 1 { one } else { many })
            })
            .collect::<Vec<_>>()
            .join(", ");
        rollups[&team][workspace.unwrap_or_default()] = json!({"mark":top,"count":counts.get(top).copied().unwrap_or_default(),"summary":summary,"total":counts.values().sum::<usize>()});
    }
    rollups
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_marks_and_availability() {
        for (status, expected) in [
            ("running", "thinking"),
            ("dropped", "question"),
            ("failed", "error"),
            ("tool-running", "tool"),
            ("crashed", "crashed"),
            ("done", "ended"),
        ] {
            assert_eq!(mark(&json!({"effectiveStatus":status})), expected);
            assert_eq!(
                mark(&json!({"effectiveStatus":status,"state":"offline"})),
                if matches!(expected, "ended" | "crashed") {
                    expected
                } else {
                    "offline"
                }
            );
        }
    }
    #[test]
    fn rollup_counts_the_winning_mark_and_keeps_all_summary_states() {
        let input = serde_json::from_value(json!({"teamKey":"org:dev","sessions":[
            {"row":{"handle":"a","sessionId":"a","kind":"dashboard-launched","alive":true},"workspace":{"id":"ws"}},
            {"row":{"handle":"b","sessionId":"b","kind":"dashboard-launched","alive":true},"workspace":{"id":"ws"}},
            {"row":{"handle":"c","sessionId":"c","kind":"dashboard-launched","alive":true},"workspace":{"id":"ws"}}
        ]})).unwrap();
        let rows = super::super::rows::rows(&input, &json!({}));
        let rollups = workspace_rollups(
            &rows,
            &[
                (None, json!({"id":"a","effectiveStatus":"idle"})),
                (None, json!({"id":"b","effectiveStatus":"idle"})),
                (
                    None,
                    json!({"id":"c","effectiveStatus":"idle","state":"offline"}),
                ),
            ],
        );
        assert_eq!(
            rollups["org:dev"]["ws"],
            json!({"mark":"offline","count":1,"total":3,"summary":"2 ready, 1 offline"})
        );
    }
}
