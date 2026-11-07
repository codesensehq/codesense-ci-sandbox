// SPDX-License-Identifier: MIT
//! The reader for `opencode run --format json`: one JSON event per stdout line.
//!
//! OpenCode does not version this stream, so the shape CodeSense relies on is pinned here
//! (read against OpenCode 2.1.4; a test pins each event). Every event carries `type` and
//! `sessionID`:
//!
//! - `step_start`: a model step begins.
//! - `text`: `part.text` is one complete text part, not a delta.
//! - `tool_use`: one event per finished tool, with its result folded in. `part.tool` names
//!   it, `part.state.input` is its input, and `part.state.status` is `completed` (result in
//!   `output`) or `error` (message in `error`). A rejected or denied call is an `error`.
//! - `step_finish`: `part.reason` is `tool-calls` while the model goes on, or `stop` for
//!   the step that ends the run; `part.tokens` holds `input` and `output` counts.
//! - `error`: `error.message` says why the run failed. The process exits non-zero for
//!   it, though the stream alone must be enough to see it.
//!
//! A line that is not JSON, or an event of another type, is skipped. A reader is only as
//! strict as it must be to give a summary, turns and usage: a changed stream degrades to
//! the plain-text log, it never fails a run.

use serde_json::Value;

use super::super::output::{self, SessionTranscript, ToolCall, Turn, Usage};

fn error_message(event: &Value) -> Option<String> {
    let error = &event["error"];
    let message = error["message"]
        .as_str()
        .or_else(|| error["type"].as_str())?
        .trim();
    (!message.is_empty()).then(|| message.to_owned())
}

/// The message of the last `error` event: the run failed, whatever the exit code says.
pub(crate) fn run_error(raw: &str) -> Option<String> {
    output::json_events(raw)
        .filter(|e| e["type"] == "error")
        .filter_map(|e| error_message(&e))
        .last()
}

/// What the run reports: its failure message, else the text of the last step that ended
/// with `stop`. `None` when the stream holds neither (a run stopped by a rejected tool).
pub(crate) fn run_summary(raw: &str) -> Option<String> {
    if let Some(error) = run_error(raw) {
        return Some(error);
    }
    let mut step: Vec<String> = Vec::new();
    let mut last_stop = None;
    for event in output::json_events(raw) {
        match event["type"].as_str() {
            Some("step_start") => step.clear(),
            Some("text") => {
                if let Some(text) = event["part"]["text"].as_str().map(str::trim) {
                    if !text.is_empty() {
                        step.push(text.to_owned());
                    }
                }
            }
            Some("step_finish") => {
                if event["part"]["reason"] == "stop" && !step.is_empty() {
                    last_stop = Some(step.join("\n"));
                }
                step.clear();
            }
            _ => {}
        }
    }
    last_stop
}

fn tool_call(part: &Value, ts_ms: Option<i64>) -> ToolCall {
    let state = &part["state"];
    let failed = state["status"] == "error";
    let result = if failed {
        state["error"].as_str()
    } else {
        state["output"].as_str()
    };
    ToolCall {
        input: state.get("input").map(Value::to_string),
        result: result.map(str::to_owned),
        is_error: Some(failed),
        native_id: part["callID"].as_str().map(str::to_owned),
        ts_ms,
        // OpenCode's own skill tool names the skill in its input.
        skill: (part["tool"] == "skill")
            .then(|| state["input"]["name"].as_str().map(str::to_owned))
            .flatten(),
        // The tool name alone does not say which MCP server a call went to.
        ..ToolCall::named(part["tool"].as_str().unwrap_or("tool"))
    }
}

/// The assistant turn of the step in progress, started on first use.
fn step_turn(turns: &mut Vec<Turn>, step: &mut Option<usize>, ts_ms: Option<i64>) -> usize {
    *step.get_or_insert_with(|| {
        turns.push(Turn {
            role: "assistant".into(),
            ts_ms,
            ..Turn::default()
        });
        turns.len() - 1
    })
}

/// The neutral turns of the run: one assistant turn per model step, holding its text and
/// tool calls and the step's token usage, then a `result` turn when the run failed.
pub(crate) fn parse(raw: &str) -> Option<SessionTranscript> {
    let mut session_id = None;
    let mut turns: Vec<Turn> = Vec::new();
    let mut step: Option<usize> = None;
    let mut failure = None;
    for event in output::json_events(raw) {
        if session_id.is_none() {
            session_id = event["sessionID"].as_str().map(str::to_owned);
        }
        let part = &event["part"];
        let ts_ms = event["timestamp"].as_i64();
        match event["type"].as_str() {
            Some("step_start") => step = None,
            Some("text") => {
                let Some(text) = part["text"].as_str().filter(|t| !t.trim().is_empty()) else {
                    continue;
                };
                let index = step_turn(&mut turns, &mut step, ts_ms);
                let turn = &mut turns[index];
                turn.text = Some(match turn.text.take() {
                    Some(earlier) => format!("{earlier}\n{text}"),
                    None => text.to_owned(),
                });
            }
            Some("tool_use") => {
                let index = step_turn(&mut turns, &mut step, ts_ms);
                turns[index].tool_calls.push(tool_call(part, ts_ms));
            }
            Some("step_finish") => {
                let tokens = &part["tokens"];
                let usage = Usage {
                    input_tokens: tokens["input"].as_u64(),
                    output_tokens: tokens["output"].as_u64(),
                };
                if usage.input_tokens.is_some() || usage.output_tokens.is_some() {
                    let index = step_turn(&mut turns, &mut step, ts_ms);
                    turns[index].usage = Some(usage);
                }
                step = None;
            }
            Some("error") => failure = error_message(&event).or(failure),
            _ => {}
        }
    }
    if let Some(message) = failure {
        turns.push(Turn {
            role: "result".into(),
            text: Some(message),
            ..Turn::default()
        });
    }
    if turns.is_empty() {
        return None;
    }
    Some(SessionTranscript::new(
        session_id,
        turns,
        // `run --format json` streams events for the run; it names no model, writes no reasoning,
        // and the user's request is the run's argument, not an event.
        [
            ("turn.model", "the run's JSON events do not name the model"),
            ("turn.reasoning", "the run's JSON events carry no reasoning"),
            ("turn.user", "the run's JSON events do not echo the request"),
        ]
        .into_iter()
        .map(|(field, reason)| (field.to_owned(), reason.to_owned()))
        .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/opencode/",
                $name
            ))
        };
    }

    const SESSION: &str = "ses_e8efa55b0ffeb98GMs6Q3lPTHm";

    #[test]
    fn a_text_step_is_one_assistant_turn_with_its_usage_and_the_session_id() {
        let t = parse(fixture!("text.jsonl")).unwrap();
        assert_eq!(t.session_id.as_deref(), Some(SESSION));
        assert_eq!(t.turns.len(), 1);
        let turn = &t.turns[0];
        assert_eq!(turn.role, "assistant");
        assert_eq!(
            turn.text.as_deref(),
            Some("FINAL-ANSWER: the fake model says hello.")
        );
        assert_eq!(
            turn.usage,
            Some(Usage {
                input_tokens: Some(123),
                output_tokens: Some(45)
            })
        );
    }

    #[test]
    fn a_completed_tool_is_a_call_with_its_result_and_the_next_step_is_its_own_turn() {
        let t = parse(fixture!("tool-completed.jsonl")).unwrap();
        assert_eq!(t.turns.len(), 2);
        let call = &t.turns[0].tool_calls[0];
        assert_eq!(call.name, "bash");
        assert!(call.input.as_deref().unwrap().contains("echo probe-ok"));
        assert_eq!(call.result.as_deref(), Some("probe-ok\n"));
        assert_eq!(call.is_error, Some(false));
        assert!(t.turns[0].usage.is_some());
        assert_eq!(
            t.turns[1].text.as_deref(),
            Some("FINAL-ANSWER: tool finished, result seen.")
        );
    }

    #[test]
    fn a_tool_call_keeps_its_native_id_and_time_and_the_gaps_are_named() {
        let t = parse(fixture!("tool-completed.jsonl")).unwrap();
        let call = &t.turns[0].tool_calls[0];
        assert_eq!(call.native_id.as_deref(), Some("call_1"));
        assert_eq!(call.ts_ms, Some(1792897571472));
        assert_eq!(t.turns[0].index, Some(0));
        for field in ["turn.model", "turn.reasoning", "turn.user"] {
            assert!(t.unavailable.contains_key(field), "{field}");
        }
    }

    #[test]
    fn opencodes_skill_tool_names_the_skill_and_no_other_tool_does() {
        let raw = [
            r#"{"type":"tool_use","timestamp":1,"sessionID":"s","part":{"type":"tool","tool":"skill","callID":"c1","state":{"status":"completed","input":{"name":"bmad-help"},"output":"ok"}}}"#,
            r#"{"type":"tool_use","timestamp":2,"sessionID":"s","part":{"type":"tool","tool":"bash","callID":"c2","state":{"status":"completed","input":{"command":"ls"},"output":"x"}}}"#,
        ]
        .join("\n");
        let t = parse(&raw).unwrap();
        let calls = &t.turns[0].tool_calls;
        assert_eq!(calls[0].skill.as_deref(), Some("bmad-help"));
        assert_eq!(calls[1].skill, None);
    }

    #[test]
    fn a_denied_pattern_is_an_error_result_and_the_run_goes_on_to_its_final_text() {
        let raw = fixture!("deny-continue.jsonl");
        let t = parse(raw).unwrap();
        let call = &t.turns[0].tool_calls[0];
        assert_eq!(call.is_error, Some(true));
        assert!(call.result.as_deref().unwrap().contains("prevents you"));
        assert_eq!(
            run_summary(raw).as_deref(),
            Some("FINAL-ANSWER: tool finished, result seen.")
        );
        assert_eq!(run_error(raw), None);
    }

    #[test]
    fn a_rejected_ask_stops_the_run_with_an_error_call_and_no_final_text() {
        let raw = fixture!("ask-rejected.jsonl");
        let t = parse(raw).unwrap();
        assert_eq!(t.turns.len(), 1);
        assert_eq!(t.turns[0].tool_calls[0].is_error, Some(true));
        assert_eq!(
            t.turns[0].tool_calls[0].result.as_deref(),
            Some("The user rejected permission to use this specific tool call.")
        );
        assert_eq!(run_summary(raw), None);
        assert_eq!(run_error(raw), None);
    }

    #[test]
    fn an_error_event_is_the_failure_and_the_summary() {
        let raw = fixture!("error-400.jsonl");
        assert_eq!(run_error(raw).as_deref(), Some("fake bad request"));
        assert_eq!(run_summary(raw).as_deref(), Some("fake bad request"));
        let t = parse(raw).unwrap();
        assert_eq!(
            t.session_id.as_deref(),
            Some("ses_e8ef8164effeb2d8IyC2s22Qs5")
        );
        assert_eq!(t.turns.len(), 1);
        assert_eq!(t.turns[0].role, "result");
        assert_eq!(t.turns[0].text.as_deref(), Some("fake bad request"));

        let raw = fixture!("error-bad-model.jsonl");
        assert_eq!(
            run_error(raw).as_deref(),
            Some("Unexpected server error. Check server logs for details.")
        );
    }

    #[test]
    fn the_summary_is_the_last_stop_steps_text_not_an_earlier_step() {
        let raw = format!(
            "{}\n{}",
            fixture!("tool-completed.jsonl").trim_end(),
            r#"{"type":"step_start","sessionID":"s","part":{}}
{"type":"text","sessionID":"s","part":{"text":"later words"}}
{"type":"step_finish","sessionID":"s","part":{"reason":"stop","tokens":{}}}"#
        );
        assert_eq!(run_summary(&raw).as_deref(), Some("later words"));
    }

    #[test]
    fn malformed_lines_and_unknown_events_are_skipped() {
        let raw = format!(
            "not json\n{{\"type\":\"surprise\"}}\n\n{{\"truncated\n{}",
            fixture!("text.jsonl")
        );
        let t = parse(&raw).unwrap();
        assert_eq!(t.turns.len(), 1);
        assert_eq!(parse("garbage\n{}\n"), None);
        assert_eq!(run_summary("garbage"), None);
        assert_eq!(run_error(""), None);
    }

    #[test]
    fn only_the_tail_of_a_long_stream_is_read() {
        let filler = "x".repeat(output::MAX_RECORD_BYTES);
        let raw = format!(
            "{{\"type\":\"error\",\"error\":{{\"data\":{{\"message\":\"old\"}}}}}}\n{filler}\n{}",
            fixture!("text.jsonl")
        );
        assert_eq!(run_error(&raw), None);
        assert_eq!(
            run_summary(&raw).as_deref(),
            Some("FINAL-ANSWER: the fake model says hello.")
        );
    }
}
