// SPDX-License-Identifier: MIT
//! The reader for `hermes chat -Q --format stream-json` (pinned 0.22.5, `hermes_cli/stream_json.py`):
//! one JSON event per stdout line, flushed as it happens.
//!
//! - `system`/`init`: carries a `session_id` that is empty at that point; it is never native identity.
//! - `text`: one streamed delta of the assistant's prose.
//! - `tool_use` / `tool_result`: a call (`name`, `tool_call_id`, `input`) and its result (`output`,
//!   `is_error`). A blocked or failed tool is `is_error` alone, never a failed run.
//! - `result`: the single terminal record: `session_id` (the only native identity), `exit_code`,
//!   `text`, `tokens`, and `error` when the turn failed. A recovered provider error leaves no frame:
//!   it surfaces only when the turn still fails.
//!
//! A run with no `result` record died before it concluded, whatever its exit code was.

use std::collections::HashMap;

use serde_json::Value;

use super::super::output::{self, SessionTranscript, ToolCall, Turn, Usage};

fn text_of(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|text| !text.is_empty())
}

/// The last terminal `result` record of the run, if it wrote one.
fn result(raw: &str) -> Option<Value> {
    output::json_events(raw).filter(|event| event["type"] == "result").last()
}

/// Why a run with this `result` (if any) failed: no result record, a terminal `error`, or a
/// non-zero native exit.
fn failure_of(result: Option<&Value>) -> Option<String> {
    let Some(result) = result else {
        return Some("no result record".into());
    };
    if let Some(error) = text_of(&result["error"]) {
        return Some(error.to_owned());
    }
    match result["exit_code"].as_i64() {
        Some(0) => None,
        Some(code) => Some(format!("exit code {code}")),
        None => Some("exit code missing".into()),
    }
}

pub(crate) fn run_failure(raw: &str) -> Option<String> {
    failure_of(result(raw).as_ref())
}

/// What the run reports: why it failed, else its final prose.
pub(crate) fn run_summary(raw: &str) -> Option<String> {
    let result = result(raw);
    failure_of(result.as_ref()).or_else(|| text_of(&result.as_ref()?["text"]).map(str::to_owned))
}

/// Assistant turns under construction: text and calls of one model step share a turn, and a
/// tool result closes the step.
#[derive(Default)]
struct Turns {
    turns: Vec<Turn>,
    open: Option<usize>,
    calls: HashMap<String, (usize, usize)>,
}

impl Turns {
    fn step(&mut self, ts_ms: Option<i64>) -> usize {
        *self.open.get_or_insert_with(|| {
            self.turns.push(Turn {
                role: "assistant".into(),
                ts_ms,
                ..Turn::default()
            });
            self.turns.len() - 1
        })
    }

    /// Add `call` to the open step, returning where it went.
    fn push_call(&mut self, call: ToolCall) -> (usize, usize) {
        let turn = self.step(call.ts_ms);
        self.turns[turn].tool_calls.push(call);
        (turn, self.turns[turn].tool_calls.len() - 1)
    }

    fn tool_use(&mut self, event: &Value) {
        let ts_ms = event["timestamp"].as_i64();
        let call = ToolCall {
            input: event.get("input").map(Value::to_string),
            native_id: event["tool_call_id"].as_str().map(str::to_owned),
            ts_ms,
            ..ToolCall::named(event["name"].as_str().unwrap_or("tool"))
        };
        let at = self.push_call(call);
        if let Some(id) = event["tool_call_id"].as_str() {
            self.calls.insert(id.to_owned(), at);
        }
    }

    /// Pair a result with its call by `tool_call_id`; a result with no known call stands alone.
    fn tool_result(&mut self, event: &Value) {
        let paired = event["tool_call_id"].as_str().and_then(|id| self.calls.get(id).copied());
        let (turn, call) = paired.unwrap_or_else(|| {
            self.push_call(ToolCall {
                ts_ms: event["timestamp"].as_i64(),
                native_id: event["tool_call_id"].as_str().map(str::to_owned),
                ..ToolCall::named(event["name"].as_str().unwrap_or("tool"))
            })
        });
        let call = &mut self.turns[turn].tool_calls[call];
        call.result = event["output"].as_str().map(str::to_owned);
        call.is_error = Some(event["is_error"].as_bool().unwrap_or(false));
        self.open = None;
    }
}

/// The neutral turns of the run: one assistant turn per model step, then a `result` turn when the
/// run failed. The session id is the result record's alone.
pub(crate) fn parse(raw: &str) -> Option<SessionTranscript> {
    let mut built = Turns::default();
    let mut texts = false;
    for event in output::json_events(raw) {
        match event["type"].as_str() {
            Some("text") => {
                if let Some(delta) = event["text"].as_str() {
                    let turn = built.step(event["timestamp"].as_i64());
                    built.turns[turn].text.get_or_insert_with(String::new).push_str(delta);
                    texts = true;
                }
            }
            Some("tool_use") => built.tool_use(&event),
            Some("tool_result") => built.tool_result(&event),
            _ => {}
        }
    }
    let mut turns = built.turns;
    for turn in &mut turns {
        turn.text = turn.text.take().map(|text| text.trim().to_owned()).filter(|text| !text.is_empty());
    }
    let result = result(raw);
    let failure = failure_of(result.as_ref());
    let session_id = result.as_ref().and_then(|r| text_of(&r["session_id"])).map(str::to_owned);
    if let Some(result) = &result {
        // A reply that streamed nothing is still the run's prose, a failed one's recovery
        // instructions included; the verdict is the separate `result` turn below.
        if !texts {
            if let Some(text) = text_of(&result["text"]) {
                turns.push(Turn { role: "assistant".into(), text: Some(text.to_owned()), ..Turn::default() });
            }
        }
        let tokens = &result["tokens"];
        let usage = Usage { input_tokens: tokens["input"].as_u64(), output_tokens: tokens["output"].as_u64() };
        if usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0) > 0 {
            if turns.last().is_none_or(|turn| turn.role != "assistant") {
                turns.push(Turn { role: "assistant".into(), ..Turn::default() });
            }
            if let Some(turn) = turns.last_mut() {
                turn.usage = Some(usage);
            }
        }
    }
    if let Some(failure) = failure.filter(|_| result.is_some()) {
        turns.push(Turn { role: "result".into(), text: Some(failure), ..Turn::default() });
    }
    if turns.is_empty() {
        return None;
    }
    Some(SessionTranscript::new(
        session_id,
        turns,
        [
            ("turn.model", "the stream's init record does not name the model"),
            ("turn.reasoning", "the stream carries no reasoning"),
            ("turn.user", "the stream does not echo the request"),
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
                "/tests/fixtures/hermes/",
                $name
            ))
        };
    }

    const RESULT_SESSION: &str =
        "local-c2853ff46dba898717a7b75d5d95beb5b113df25e7d27f027871fb7786790cea";

    fn without_result(raw: &str) -> String {
        raw.lines()
            .filter(|line| !line.contains("\"type\": \"result\""))
            .map(|line| format!("{line}\n"))
            .collect()
    }

    /// Every recorded input against the three readers: (name, raw, failure, summary, session).
    #[test]
    fn each_recorded_run_has_its_failure_summary_and_native_identity() {
        let missing = without_result(fixture!("text.jsonl"));
        let nonzero = fixture!("text.jsonl").replace("\"exit_code\": 0", "\"exit_code\": 2");
        let rows: [(&str, &str, Option<&str>, Option<&str>, Option<&str>); 7] = [
            ("text", fixture!("text.jsonl"), None, Some("PRB_FINISHED"), Some(RESULT_SESSION)),
            ("denied then result 0", fixture!("deny-continue.jsonl"), None, Some("PRB_FINISHED"), None),
            ("recovered 5xx", fixture!("recover-5xx.jsonl"), None, Some("PRB_FINISHED"), None),
            ("missing result", &missing, Some("no result record"), Some("no result record"), None),
            ("deadline: init only", fixture!("error-5xx-timeout.jsonl"), Some("no result record"), Some("no result record"), None),
            ("result exit_code 2", &nonzero, Some("exit code 2"), Some("exit code 2"), None),
            ("terminal result.error", fixture!("error-4xx.jsonl"), Some("HTTP 400: fake bad request"), Some("HTTP 400: fake bad request"), None),
        ];
        for (name, raw, failure, summary, session) in rows {
            assert_eq!(run_failure(raw).as_deref(), failure, "{name} failure");
            assert_eq!(run_summary(raw).as_deref(), summary, "{name} summary");
            if let Some(session) = session {
                assert_eq!(parse(raw).unwrap().session_id.as_deref(), Some(session), "{name}");
            }
        }
    }

    #[test]
    fn identity_comes_only_from_the_result_and_prose_is_transcript_evidence() {
        let t = parse(&without_result(fixture!("text.jsonl")).replace("\"session_id\": \"\"", "\"session_id\": \"from-init\"")).unwrap();
        assert_eq!(t.session_id, None, "system.session_id is not native identity");
        let t = parse(fixture!("text.jsonl")).unwrap();
        assert_eq!(t.turns.len(), 1);
        assert_eq!(t.turns[0].role, "assistant");
        assert_eq!(t.turns[0].text.as_deref(), Some("PRB_FINISHED"));
        assert_eq!(t.turns[0].usage.as_ref().and_then(|u| u.input_tokens), Some(10));
    }

    #[test]
    fn a_denied_call_pairs_by_id_and_is_not_a_run_failure() {
        let raw = fixture!("deny-continue.jsonl");
        let t = parse(raw).unwrap();
        let call = &t.turns[0].tool_calls[0];
        assert_eq!(call.name, "terminal");
        assert_eq!(call.native_id.as_deref(), Some("prb_0"));
        assert_eq!(call.is_error, Some(true));
        assert!(call.result.as_deref().unwrap().contains("BLOCKED"));
        assert_eq!(t.turns[1].text.as_deref(), Some("PRB_FINISHED"));
        assert_eq!(run_failure(raw), None);
    }

    #[test]
    fn a_failed_run_ends_with_a_result_turn_and_only_the_tail_is_read() {
        let t = parse(fixture!("error-4xx.jsonl")).unwrap();
        let last = t.turns.last().unwrap();
        assert_eq!(last.role, "result");
        assert_eq!(last.text.as_deref(), Some("HTTP 400: fake bad request"));
        // The native recovery instructions stay as transcript evidence, once.
        let prose: Vec<_> = t.turns.iter().filter(|turn| turn.role == "assistant").collect();
        assert_eq!(prose.len(), 1);
        assert!(prose[0].text.as_deref().unwrap().contains("Start a clean session with /new"));
        let filler = "x".repeat(super::super::super::output::MAX_RECORD_BYTES);
        let raw = format!("{}\n{filler}\n{}", fixture!("error-4xx.jsonl").trim_end(), fixture!("text.jsonl"));
        assert_eq!(run_failure(&raw), None, "the old error is outside the bounded tail");
        assert_eq!(run_failure("garbage\n{}\n").as_deref(), Some("no result record"));
        assert_eq!(parse("garbage\n{}\n"), None);
    }
}
