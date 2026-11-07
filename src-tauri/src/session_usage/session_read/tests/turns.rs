// SPDX-License-Identifier: MIT
//! SV-CDE-91 E3: `include: [turns]` pages the stored turns as summaries.
use super::*;
use std::path::PathBuf;
use crate::session_conversation::StoredConversation;
use crate::session_usage::conversation::{ConversationMeta, State};
use codesense_pty_core::agent_cli::output::{SessionTranscript, ToolCall, Turn};

const SECRET: &str = "sk-live-SEEDED-SECRET-4242";

/// A session of `count` turns that each call two tools (the first fails on every fifth turn);
/// every call's input and result hold the seeded secret.
fn seed(dir: &Path, s: &mut UsageStore, count: u32, state: State) -> (PathBuf, PathBuf) {
    let path = dir.join("usage.sqlite");
    let conv = dir.join("conversations");
    let mut r = SessionRecord::new("tab");
    r.cli = Some("claude".into());
    r.provided_skills = Some(vec!["skill-never-listed".into()]);
    s.write_session_record(&r).unwrap();
    s.write_conversation(
        &ConversationMeta {
            session_id: "tab".into(),
            state,
            source_format: None,
            bytes: 0,
            turn_count: count,
            truncated: None,
            captured_at_ms: 1,
            unavailable: Default::default(),
        },
        &[],
    )
    .unwrap();
    std::fs::create_dir_all(&conv).unwrap();
    let call = |name: &str, failed: bool| ToolCall {
        name: name.into(),
        input: Some(format!("curl -H 'Authorization: {SECRET}' https://example.test")),
        result: Some(format!("token={SECRET}")),
        is_error: Some(failed),
        ..Default::default()
    };
    let turns = (0..count)
        .map(|i| Turn {
            role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
            text: Some("word ".repeat(120)),
            tool_calls: vec![call("Bash", i % 5 == 0), call("Read", false)],
            index: Some(i),
            ts_ms: Some(1_790_000_000_000 + i64::from(i)),
            model: Some("claude-sonnet-5-5".into()),
            ..Default::default()
        })
        .collect();
    let stored = StoredConversation {
        session_id: "tab".into(),
        source_format: "fixture".into(),
        captured_at_ms: 1,
        truncated: None,
        transcript: SessionTranscript::new(Some("tab".into()), turns, Default::default()),
    };
    std::fs::write(conv.join("tab.json"), serde_json::to_vec(&stored).unwrap()).unwrap();
    (path, conv)
}

async fn turns(path: &Path, conv: &Path, args: Value) -> Value {
    let p: Params = serde_json::from_value(args).unwrap();
    read(path, conv, &p, &CallerScope::Human).await.unwrap()
}

/// AC3: a 45-turn session answers under 16 KB, with no tool input or result text (a seeded
/// secret in both never appears), no provided skills, and the summary fields only.
#[tokio::test]
async fn a_45_turn_answer_is_light_and_holds_no_tool_text() {
    let (dir, mut s) = store();
    let (path, conv) = seed(dir.path(), &mut s, 45, State::Present);
    let got = turns(&path, &conv, json!({"sessionId":"tab","include":["turns"],"limit":200})).await;
    let text = serde_json::to_string(&got).unwrap();
    assert!(text.len() < 16 * 1024, "{} bytes", text.len());
    assert!(!text.contains(SECRET), "a tool input or result reached the answer");
    assert!(!text.contains("skill-never-listed") && !text.contains("providedSkills"));
    assert!(!text.contains("curl"), "a tool input reached the answer");
    let rows = got["turns"]["turns"].as_array().unwrap();
    assert_eq!(rows.len(), 45);
    assert_eq!(got["turns"]["state"], "present");
    let first = rows[0].as_object().unwrap();
    let mut keys: Vec<&str> = first.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["index", "model", "role", "text", "toolCalls", "toolErrors", "tsMs"]);
    assert_eq!(rows[0]["toolCalls"], 2);
    assert_eq!(rows[0]["toolErrors"], 1, "turn 0 failed its first call");
    assert_eq!(rows[1]["toolErrors"], 0);
    assert!(rows.iter().all(|row| row["text"].as_str().unwrap().chars().count() <= 200));
    assert_eq!(got["session"]["sessionId"], "tab");
    assert_eq!(got["session"]["cli"], "claude");
    // The reasons for what the light answer holds (no timing, story link or record) stay with it.
    assert!(got["unavailable"]["story"].is_string() && got["unavailable"]["timing"].is_string());
    assert!(got["unavailable"].get("retries").is_none());
    assert!(got.get("conversation").is_none() && got.get("tools").is_none());
}

/// AC3, any text: 45 turns of non-ASCII text, quotes and control characters stay under 16 KB, since
/// a summary is bounded in escaped JSON bytes as well as in chars.
#[tokio::test]
async fn a_45_turn_answer_of_any_text_stays_under_sixteen_kilobytes() {
    for text in ["語".repeat(300), "\"".repeat(300), "\u{1}".repeat(300), "😀".repeat(300)] {
        let (dir, mut s) = store();
        let (path, conv) = seed(dir.path(), &mut s, 45, State::Present);
        let mut stored: StoredConversation =
            serde_json::from_slice(&std::fs::read(conv.join("tab.json")).unwrap()).unwrap();
        for turn in &mut stored.transcript.turns {
            turn.text = Some(text.clone());
        }
        std::fs::write(conv.join("tab.json"), serde_json::to_vec(&stored).unwrap()).unwrap();
        let got = turns(&path, &conv, json!({"sessionId":"tab","include":["turns"],"limit":200})).await;
        let size = serde_json::to_string(&got).unwrap().len();
        assert!(size < 16 * 1024, "{size} bytes for {:?}", text.chars().next());
        let shown = got["turns"]["turns"][0]["text"].as_str().unwrap();
        assert!(shown.chars().count() <= 200 && !shown.is_empty());
    }
}

/// A summary's byte count is what `serde_json` writes for it: the escape widths in `summary_text`
/// match the real serializer, for ASCII, quotes, backslashes, control characters, non-ASCII and emoji.
#[test]
fn summary_text_counts_the_bytes_serde_json_writes() {
    let sample: String = ["plain ", "\"quoted\" ", "back\\slash ", "tab\t nl\n cr\r ", "\u{1} \u{8} \u{c} \u{1f} ", "語語語 ", "😀😀 ", "é "]
        .concat()
        .repeat(8);
    for take in [1, 5, 40, 120, 400] {
        let text: String = sample.chars().take(take).collect();
        let shown = summary_text(&text);
        let written = serde_json::to_string(&shown).unwrap();
        assert!(written.len() - 2 <= SUMMARY_BYTES, "{take}: {} bytes", written.len() - 2);
        assert!(text.starts_with(&shown), "{take}: a prefix of the text");
        // Cut as late as the cap allows: the next char would have passed it.
        if let Some(next) = text[shown.len()..].chars().next() {
            let with_next = serde_json::to_string(&format!("{shown}{next}")).unwrap();
            assert!(with_next.len() - 2 > SUMMARY_BYTES, "{take}: stopped early at {next:?}");
        }
    }
}

/// The turns page is the conversation pager: same cursor, same limit, same states.
#[tokio::test]
async fn turns_page_and_report_states_like_the_conversation() {
    let (dir, mut s) = store();
    let (path, conv) = seed(dir.path(), &mut s, 45, State::Present);
    let page = turns(&path, &conv, json!({"sessionId":"tab","include":["turns"],"limit":20})).await;
    assert_eq!(page["turns"]["turns"].as_array().unwrap().len(), 20);
    assert_eq!(page["turns"]["nextCursor"], 20);
    let next = turns(&path, &conv, json!({"sessionId":"tab","include":["turns"],"cursor":40,"limit":20})).await;
    assert_eq!(next["turns"]["turns"].as_array().unwrap().len(), 5);
    assert!(next["turns"]["nextCursor"].is_null());
    assert_eq!(next["turns"]["turns"][0]["index"], 40);
    let full = turns(&path, &conv, json!({"sessionId":"tab","include":["conversation"],"cursor":40,"limit":20})).await;
    assert_eq!(next["turns"]["turnCount"], full["conversation"]["turnCount"]);
    assert_eq!(next["turns"]["state"], full["conversation"]["state"]);
    // No stored conversation: the state and reason are the pager's, with no turns.
    let (dir, mut s) = store();
    s.write_session_record(&SessionRecord::new("tab")).unwrap();
    let path = dir.path().join("usage.sqlite");
    let none = turns(&path, &dir.path().join("conversations"), json!({"sessionId":"tab","include":["turns"]})).await;
    assert_eq!(none["turns"]["turns"], json!([]));
    assert!(none["turns"]["reason"].is_string());
}

/// A call whose failure the record does not say is unknown, never a success: it is counted apart.
#[tokio::test]
async fn a_call_with_no_recorded_outcome_is_counted_unknown() {
    let (dir, mut s) = store();
    let (path, conv) = seed(dir.path(), &mut s, 1, State::Present);
    let mut stored: StoredConversation =
        serde_json::from_slice(&std::fs::read(conv.join("tab.json")).unwrap()).unwrap();
    stored.transcript.turns[0].tool_calls[1].is_error = None;
    std::fs::write(conv.join("tab.json"), serde_json::to_vec(&stored).unwrap()).unwrap();
    let got = turns(&path, &conv, json!({"sessionId":"tab","include":["turns"]})).await;
    let row = &got["turns"]["turns"][0];
    assert_eq!((&row["toolErrors"], &row["toolErrorsUnknown"]), (&json!(1), &json!(1)));
}

#[test]
fn turns_and_conversation_together_are_invalid_and_selectors_need_one() {
    let both: Params = serde_json::from_value(json!({"sessionId":"tab","include":["conversation","turns"]})).unwrap();
    assert!(both.selector().unwrap_err().starts_with("invalid:"));
    let bare: Params = serde_json::from_value(json!({"sessionId":"tab","limit":5})).unwrap();
    assert!(bare.selector().unwrap_err().contains("[turns]"));
}
