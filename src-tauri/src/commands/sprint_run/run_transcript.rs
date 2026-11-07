// SPDX-License-Identifier: MIT
//! SW-OR-16 — adapter-declared **structured** transcript: the optional turn-by-turn upgrade on top of
//! SW-OR-12's plain-text floor (`run_artifact.rs`).
//!
//! An adapter that declares `structured_transcript` requests a structured source — the
//! headless `stream-json` NDJSON on stdout, or the agent's own on-disk session JSONL — and names its
//! [`TranscriptFormat`]. This module maps that source, through a **format-keyed** parser (no core `if agent`
//! branch), into an **agent-neutral** turn shape: turns / tool calls / tool results / per-turn usage.
//!
//! Progressive + safe by construction:
//! - **Off the render path** — parsing runs trusted-side; the FE reads already-parsed [`Turn`]s, never
//!   raw NDJSON (matches OR-12's read-once/off-thread discipline).
//! - **Scrub before persist** — the raw source is redacted ([`crate::shared::secret_scan`]) BEFORE the
//!   text floor or the turns are written, independent of parse success (AC-4).
//! - **Degrade, never crash** — no declaration, or a source that yields no turns
//!   → the run stays on the text floor; a malformed line is skipped, a wholly unparseable source
//!   returns `None` (AC-2 / AC-5).
//! - **Additive** — the turns persist to a SEPARATE `{run_id}.turns.json`; OR-12's `{run_id}.log`
//!   write path is byte-identical for a non-declaring adapter.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use codesense_pty_core::agent_cli::TranscriptFormat;

use super::run_artifact::{run_artifacts_dir, valid_run_id, RunArtifactKind};
use crate::commands::await_blocking;
use crate::shared::secret_scan::redact_secrets;

/// The parse-input byte ceiling — the parser reads only the **tail** of the structured source, so the
/// resulting `{run_id}.turns.json` (and the IPC payload the panel deserializes) is bounded the SAME way
/// SW-OR-12 bounds the `.log` floor (a long run can't inflate the turns file without limit). The tail
/// holds the run's end (the outcome/result), so it is the signal to keep. A leading partial line left
/// by the cut is skipped by the lenient per-line parse.
const PARSE_MAX_BYTES: usize = 1024 * 1024;

pub use codesense_pty_core::agent_cli::output::{
    Reasoning, ReasoningKind, SessionTranscript, ToolCall, Turn, Usage,
};

/// Dispatch a declared `format` to its parser, mapping `raw` → neutral turns. A source that yields
/// no turns → `None` (text fallback). Both bundled Anthropic formats share the
/// NDJSON-of-API-messages core; the format still gates so a genuinely different future format is a
/// new arm, never an `if agent` branch on the hot path.
pub fn parse_transcript(format: TranscriptFormat, raw: &str) -> Option<SessionTranscript> {
    match format {
        TranscriptFormat::AnthropicStreamJson | TranscriptFormat::AnthropicSessionJsonl => {
            // Bound the parse input to the tail (mirrors the `.log` cap) so a multi-MB run can't
            // inflate the turns file / IPC payload without limit.
            let bounded = super::run_artifact::tail_bounded(raw.as_bytes(), PARSE_MAX_BYTES);
            parse_transcript_lines(format, bounded.lines())
        }
        native => native.parse_native(raw),
    }
}

/// [`parse_transcript`] over every line one at a time (each already redacted) with no tail bound,
/// so the session conversation store never holds a long native log whole in memory.
pub fn parse_transcript_lines<I>(format: TranscriptFormat, lines: I) -> Option<SessionTranscript>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    match format {
        TranscriptFormat::AnthropicStreamJson | TranscriptFormat::AnthropicSessionJsonl => {
            let turns = parse_message_lines(lines);
            (!turns.is_empty())
                .then(|| SessionTranscript::new(None, turns, anthropic_unavailable()))
        }
        native => native.parse_native_lines(lines),
    }
}

/// What the Anthropic session record does not hold, as facts for readers.
fn anthropic_unavailable() -> std::collections::BTreeMap<String, String> {
    [(
        "turn.cost".to_owned(),
        "the Claude session record holds token usage but no cost".to_owned(),
    )]
    .into()
}

/// SW-OR-16 (AC-6): derive the run summary from the structured **`result` event** (the agent's own
/// final-result record) rather than clamping raw NDJSON text. `None` when the source carries no
/// usable result string, so the caller falls back to today's text clamp.
pub fn result_summary(format: TranscriptFormat, raw: &str) -> Option<String> {
    if format != TranscriptFormat::AnthropicStreamJson {
        return format.native_result_summary(raw);
    }
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(serde_json::Value::as_str) == Some("result") {
            if let Some(s) = v.get("result").and_then(serde_json::Value::as_str) {
                let s = s.trim();
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

/// Why the run's own structured record says it failed, even when its process exited 0.
pub fn run_failure(format: TranscriptFormat, raw: &str) -> Option<String> {
    format.native_run_failure(raw)
}

/// The shared NDJSON-of-API-messages parser (both bundled Anthropic formats). Each non-blank line is
/// one JSON object; a line that fails to parse is **skipped** (lenient — a format-version surprise
/// drops to the floor, never crashes). `assistant` lines yield an assistant turn (text + `tool_use`);
/// `user` lines carry `tool_result` blocks that attach to the matching `tool_use` by id; everything
/// else (`system`/`result`/unknown) is ignored for the turn list.
fn parse_message_lines<I>(lines: I) -> Vec<Turn>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut turns: Vec<Turn> = Vec::new();
    let mut held: Option<Turn> = None;
    // tool_use_id → (turn index, tool_call index) so a later tool_result finds its call.
    let mut pending: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();

    for line in lines {
        let line = line.as_ref().trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = v.get("type").and_then(serde_json::Value::as_str);
        // Both stream-json and session JSONL wrap the API message under `message`; tolerate a bare
        // `content` too (defensive against a shape that inlines it).
        let message = v.get("message").unwrap_or(&v);
        match kind {
            Some("assistant") => {
                if let Some(mut turn) = assistant_turn(&v, message, &mut turns, &mut pending) {
                    if let Some(earlier) = held.take() {
                        merge_reasoning_step(&mut turn, earlier);
                    }
                    // A message that holds only the model's reasoning waits for the message that
                    // follows it (Claude writes each content block as its own row).
                    if turn.text.is_none() && turn.tool_calls.is_empty() {
                        held = Some(turn);
                    } else {
                        turns.push(turn);
                    }
                }
            }
            Some("user") => {
                attach_tool_results(message, &mut turns, &pending);
                if let Some(turn) = human_user_turn(&v, message) {
                    turns.extend(held.take());
                    turns.push(turn);
                }
            }
            _ => {}
        }
    }
    turns.extend(held);
    turns
}

/// Fold the reasoning-only message `earlier` into the assistant message `turn` that follows it:
/// the reasoning, and the first facts it has that `turn` lacks.
fn merge_reasoning_step(turn: &mut Turn, earlier: Turn) {
    turn.reasoning = turn.reasoning.take().or(earlier.reasoning);
    turn.model = turn.model.take().or(earlier.model);
    turn.ts_ms = earlier.ts_ms.or(turn.ts_ms);
    turn.usage = turn.usage.take().or(earlier.usage);
}

/// When a session-log row was written.
fn row_ts_ms(row: &serde_json::Value) -> Option<i64> {
    row.get("timestamp")
        .and_then(serde_json::Value::as_str)
        .and_then(codesense_pty_core::agent_cli::output::rfc3339_ms)
}

/// The user turn a session-log `user` row carries, when a person typed it. A row that answers a tool
/// (`tool_result` blocks, a `toolUseResult`), a meta or sidechain row, or one the CLI marks as not
/// human-originated is not the person's request and makes no turn.
fn human_user_turn(row: &serde_json::Value, message: &serde_json::Value) -> Option<Turn> {
    let flag = |key: &str| row.get(key).and_then(serde_json::Value::as_bool) == Some(true);
    if flag("isMeta")
        || flag("isSidechain")
        || row.get("toolUseResult").is_some_and(|v| !v.is_null())
    {
        return None;
    }
    if let Some(kind) = row
        .pointer("/origin/kind")
        .and_then(serde_json::Value::as_str)
    {
        if kind != "human" {
            return None;
        }
    }
    let text = match message.get("content")? {
        serde_json::Value::String(s) => join_non_empty(vec![s.clone()]),
        serde_json::Value::Array(blocks) => {
            if blocks
                .iter()
                .any(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("tool_result"))
            {
                return None;
            }
            join_non_empty(
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(serde_json::Value::as_str))
                    .map(str::to_string)
                    .collect(),
            )
        }
        _ => None,
    }?;
    Some(Turn {
        role: "user".to_string(),
        text: Some(text),
        ts_ms: row_ts_ms(row),
        ..Turn::default()
    })
}

/// Build an assistant turn from an API message: join `text` blocks, collect `tool_use` blocks as
/// [`ToolCall`]s, and register each tool_use id in `pending` so its result can attach later. Returns
/// `None` when the message has no renderable content (nothing to show).
fn assistant_turn(
    row: &serde_json::Value,
    message: &serde_json::Value,
    turns: &mut [Turn],
    pending: &mut std::collections::HashMap<String, (usize, usize)>,
) -> Option<Turn> {
    let content = message.get("content").and_then(serde_json::Value::as_array);
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let turn_index = turns.len();
    let ts_ms = row_ts_ms(row);
    let mut reasoning: Option<Reasoning> = None;

    if let Some(blocks) = content {
        for block in blocks {
            match block.get("type").and_then(serde_json::Value::as_str) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(serde_json::Value::as_str) {
                        text_parts.push(t.to_string());
                    }
                }
                Some("thinking") => {
                    // Claude writes the block with an empty `thinking` and a signature unless it
                    // shows a summary, so most turns hold a reasoning step with no text.
                    let text = block
                        .get("thinking")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|t| join_non_empty(vec![t.to_string()]));
                    reasoning = Some(Reasoning {
                        kind: if text.is_some() {
                            ReasoningKind::Summary
                        } else {
                            ReasoningKind::None
                        },
                        text,
                    });
                }
                Some("tool_use") => {
                    let name = block
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("tool")
                        .to_string();
                    let input = block.get("input").map(render_json);
                    let native_id = block
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                    if let Some(id) = block.get("id").and_then(serde_json::Value::as_str) {
                        // First-wins: if an id repeats (retry, or a format that reuses ids), keep the
                        // earliest use so its `tool_result` (which follows in emission order) attaches to
                        // the right call rather than a later one silently clobbering the mapping.
                        pending
                            .entry(id.to_string())
                            .or_insert((turn_index, tool_calls.len()));
                    }
                    // Claude's own skill tool names the skill in its input.
                    let skill = (name == "Skill")
                        .then(|| {
                            block
                                .pointer("/input/skill")
                                .and_then(serde_json::Value::as_str)
                        })
                        .flatten()
                        .map(str::to_string);
                    tool_calls.push(ToolCall {
                        input,
                        native_id,
                        ts_ms,
                        skill,
                        ..ToolCall::named(name)
                    });
                }
                _ => {}
            }
        }
    }

    let text = join_non_empty(text_parts);
    if text.is_none() && tool_calls.is_empty() && reasoning.is_none() {
        return None;
    }
    Some(Turn {
        role: "assistant".to_string(),
        text,
        tool_calls,
        usage: Usage::from_value(message),
        ts_ms,
        model: message
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        reasoning,
        ..Turn::default()
    })
}

/// Attach each `tool_result` block in a user message to the `tool_use` it answers (by `tool_use_id`),
/// filling that [`ToolCall`]'s `result` + `is_error`. A result with no known id is dropped (its call
/// never appeared — nothing to show it under), keeping the turn list coherent.
fn attach_tool_results(
    message: &serde_json::Value,
    turns: &mut [Turn],
    pending: &std::collections::HashMap<String, (usize, usize)>,
) {
    let Some(blocks) = message.get("content").and_then(serde_json::Value::as_array) else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(serde_json::Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(id) = block.get("tool_use_id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(&(ti, ci)) = pending.get(id) else {
            continue;
        };
        if let Some(call) = turns.get_mut(ti).and_then(|t| t.tool_calls.get_mut(ci)) {
            call.result = tool_result_text(block.get("content"));
            // A result block without the flag is a success: Claude writes it only on a failure.
            call.is_error = Some(
                block
                    .get("is_error")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            );
        }
    }
}

/// A `tool_result`'s `content` is either a plain string or an array of `{type:"text",text}` blocks;
/// flatten either into one string.
fn tool_result_text(content: Option<&serde_json::Value>) -> Option<String> {
    match content {
        Some(serde_json::Value::String(s)) => join_non_empty(vec![s.clone()]),
        Some(serde_json::Value::Array(blocks)) => {
            let parts: Vec<String> = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect();
            join_non_empty(parts)
        }
        _ => None,
    }
}

/// Render a tool `input` value as compact JSON (a string value stays bare). Bounded elsewhere by the
/// overall artifact size; here we just make it displayable.
fn render_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn join_non_empty(parts: Vec<String>) -> Option<String> {
    let joined = parts.join("\n");
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// ── Persistence (additive; OR-12's `.log` path is untouched for non-declaring adapters) ────────────
// Reuses SW-OR-12's `run_artifacts_dir` + `valid_run_id` guard, so the structured artifact resolves
// the SAME managed dir and inherits the SAME anti-forgery allowlist — no second copy of either.

/// The per-run structured filename `{run_id}.turns.json`, or `None` for a non-minted `run_id`.
fn turns_filename(run_id: &str) -> Option<String> {
    RunArtifactKind::Turns.filename(run_id)
}

fn write_turns_to(dir: &Path, run_id: &str, transcript: &SessionTranscript) -> Option<String> {
    let filename = turns_filename(run_id)?;
    let body = serde_json::to_string(transcript).ok()?;
    // SW-AU-82A (AC-3b): a GT-24 retry reuses the run_id, so rotate a prior attempt's turns to
    // its archive slot before the latest overwrites the legacy name — no prior evidence lost.
    super::run_artifact::archive_prior_attempt(
        dir,
        run_id,
        super::run_artifact::RunArtifactKind::Turns,
    );
    // Reuse the ONE shared atomic writer (create-parent + unique-temp + rename + cleanup) rather than
    // hand-rolling temp/rename — a reader opening the panel mid-completion sees the file whole or not
    // at all, and the `.tmp.` infix keeps the content-watcher's transient guard from firing.
    match crate::shared::fs_atomic::write_bytes_atomic(&dir.join(&filename), body.as_bytes()) {
        Ok(()) => Some(format!("run-artifacts/{run_id}.turns.json")),
        Err(e) => {
            log::warn!(target: "run_transcript", "turns write failed for {run_id}: {e}");
            None
        }
    }
}

/// SW-AU-82A (AC-6): scrub a parsed transcript at the render boundary, reusing the ONE
/// `redact_secrets` routine over each turn's data-bearing fields (prose + tool input/result).
/// Turns are built from redacted text at persist, so this is idempotent defense-in-depth that
/// also covers a turns file predating the persist scrub or one hand-edited on disk.
pub(crate) fn redact_transcript(transcript: &mut SessionTranscript) {
    for turn in &mut transcript.turns {
        if let Some(text) = &turn.text {
            turn.text = Some(redact_secrets(text));
        }
        for call in &mut turn.tool_calls {
            if let Some(input) = &call.input {
                call.input = Some(redact_secrets(input));
            }
            if let Some(result) = &call.result {
                call.result = Some(redact_secrets(result));
            }
        }
    }
}

fn read_turns_from(dir: &Path, run_id: &str) -> Option<SessionTranscript> {
    let filename = turns_filename(run_id)?;
    match std::fs::read_to_string(dir.join(&filename)) {
        Ok(body) => match serde_json::from_str(&body) {
            Ok(mut transcript) => {
                redact_transcript(&mut transcript);
                Some(transcript)
            }
            // A corrupt/hand-edited `.turns.json` degrades to the text floor like an absent one, but log
            // it — the cause (bad file) differs from the benign "this agent has no structured turns".
            Err(e) => {
                log::warn!(target: "run_transcript", "corrupt turns.json for {run_id}: {e}");
                None
            }
        },
        Err(_) => None,
    }
}

pub(super) fn materialized_transcript_run_ids_in(dir: &Path) -> HashSet<String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return HashSet::new(),
        Err(error) => {
            log::warn!(target: "run_transcript", "transcript inventory failed for {}: {error}", dir.display());
            return HashSet::new();
        }
    };
    let mut run_ids = HashSet::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log::warn!(target: "run_transcript", "transcript inventory entry failed: {error}");
                continue;
            }
        };
        let Ok(file_type) = entry.file_type() else {
            log::warn!(target: "run_transcript", "transcript inventory could not inspect {}", entry.path().display());
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        let Some(run_id) = name.strip_suffix(".turns.json") else {
            continue;
        };
        if valid_run_id(run_id) {
            run_ids.insert(run_id.to_owned());
        }
    }
    run_ids
}

pub(crate) fn materialized_transcript_run_ids() -> HashSet<String> {
    run_artifacts_dir()
        .map(|dir| materialized_transcript_run_ids_in(&dir))
        .unwrap_or_default()
}

/// SW-OR-16 (AC-2/AC-3): read a run's structured turns by opaque `run_id` — the panel's rich-mode
/// source. `None` → the panel stays on the text body (OR-12's `read_run_artifact`). Off the async
/// runtime; only `{codesense_home}/run-artifacts/` is read; a forged `run_id` is rejected.
#[tauri::command]
pub async fn read_run_transcript(run_id: String) -> Result<Option<SessionTranscript>, String> {
    await_blocking("run_transcript.read", move || {
        let sessions = super::run_store::session_ids_for_run(&run_id);
        Ok(read_transcript_for_run_from(
            run_artifacts_dir().as_deref(),
            crate::session_conversation::conversations_dir().as_deref(),
            &run_id,
            &sessions,
        ))
    })
    .await
}

/// A run's transcript: the stored conversation of the latest session it names that has one (the
/// ONE capture path, keyed by session), else the `{run_id}.turns.json` an older run wrote.
pub(crate) fn read_transcript_for_run_from(
    run_dir: Option<&Path>,
    conversations_dir: Option<&Path>,
    run_id: &str,
    sessions: &[String],
) -> Option<SessionTranscript> {
    if let Some(dir) = conversations_dir {
        for session in sessions.iter().rev() {
            if let Some(mut stored) = crate::session_conversation::read_stored(dir, session) {
                redact_transcript(&mut stored.transcript);
                return Some(stored.transcript);
            }
        }
    }
    run_dir.and_then(|dir| read_turns_from(dir, run_id))
}

/// SW-OR-16 (AC-4/AC-2): for a run whose adapter declared headless structured output, **scrub then
/// persist** — redact the raw stdout (unredacted `stream-json` tool I/O) BEFORE anything is written,
/// write the redacted text floor to `{run_id}.log` (reusing OR-12's bound + atomic write), then parse
/// the redacted stream into turns and write `{run_id}.turns.json` when any turns result. All fs off
/// the async runtime. Returns the `.log` `artifact_ref` (the outcome pointer), same as the plain path.
pub async fn persist_structured_headless_run(
    run_id: &str,
    format: TranscriptFormat,
    stdout: &[u8],
    stderr: &[u8],
) -> Option<String> {
    let Some(dir) = run_artifacts_dir() else {
        log::warn!(target: "run_transcript", "no codesense home — skipping structured persist for {run_id}");
        return None;
    };
    let run_id = run_id.to_string();
    let stdout = stdout.to_vec();
    let stderr = stderr.to_vec();
    await_blocking("run_transcript.persist", move || {
        Ok::<_, String>(persist_structured_to(
            &dir, &run_id, format, &stdout, &stderr,
        ))
    })
    .await
    .unwrap_or(None)
}

/// The pure `dir`-injected core of [`persist_structured_headless_run`] — **scrub then persist**. Redact
/// the raw stdout/stderr (unredacted `stream-json` tool I/O) BEFORE anything is written, write the
/// redacted text floor to `{run_id}.log` (reusing OR-12's bound + atomic write), then parse the redacted
/// stream into turns and write `{run_id}.turns.json` when any turns result. Redaction runs independent
/// of parse success (AC-4): the text floor of a structured run IS the NDJSON, so it must be scrubbed
/// too. Pure over `dir` so the redact→persist composition is tempdir-testable (AC-4 end-to-end).
fn persist_structured_to(
    dir: &Path,
    run_id: &str,
    format: TranscriptFormat,
    stdout: &[u8],
    stderr: &[u8],
) -> Option<String> {
    let redacted_stdout = redact_secrets(&String::from_utf8_lossy(stdout));
    let redacted_stderr = redact_secrets(&String::from_utf8_lossy(stderr));
    let artifact_ref = super::run_artifact::write_artifact_text(
        dir,
        run_id,
        redacted_stdout.as_bytes(),
        redacted_stderr.as_bytes(),
    );
    // Delegate the turns half to the shared core, reusing the floor's redaction (no second scrub).
    // A format whose CLI keeps a session log has its turns stored by session, not by run.
    if !stored_by_session(format) {
        write_parsed_turns(dir, run_id, format, &redacted_stdout);
    }
    artifact_ref
}

/// Whether a format's turns are read from the session conversation store, not written per run.
fn stored_by_session(format: TranscriptFormat) -> bool {
    !matches!(
        format,
        TranscriptFormat::OpenCodeRunJson | TranscriptFormat::HermesStreamJson
    )
}

/// The shared parse→write-turns core over **already-redacted** text: parse `redacted` via the
/// format-keyed parser and write `{run_id}.turns.json`, returning that artifact ref — or `None` when
/// nothing parses (text fallback) or the write fails. Callers scrub BEFORE calling: the headless
/// floor ([`persist_structured_to`]) reuses its single redaction; the interactive teardown redacts
/// via [`persist_turns`]. One parse+write core, no duplicated redaction.
fn write_parsed_turns(
    dir: &Path,
    run_id: &str,
    format: TranscriptFormat,
    redacted: &str,
) -> Option<String> {
    let transcript = parse_transcript(format, redacted)?;
    write_turns_to(dir, run_id, &transcript)
}

/// A native source is already bounded by its owning host. Scrub before either
/// artifact is persisted, independently of whether its structured parser succeeds.
fn persist_session_record_to(
    dir: &Path,
    run_id: &str,
    record: &codesense_pty_core::agent_cli::output::SessionRecord,
) -> Option<String> {
    let bounded = super::run_artifact::tail_bounded(record.raw.as_bytes(), PARSE_MAX_BYTES);
    let redacted = redact_secrets(&bounded);
    if let Some(mut transcript) = parse_transcript(record.format, &redacted) {
        transcript.session_id = Some(record.session_id.clone());
        write_turns_to(dir, run_id, &transcript)
    } else {
        super::run_artifact::write_artifact_text(dir, run_id, redacted.as_bytes(), &[])
    }
}

pub(crate) async fn persist_session_record(
    run_id: &str,
    record: codesense_pty_core::agent_cli::output::SessionRecord,
    directory: Option<PathBuf>,
) -> Option<String> {
    let dir = directory.or_else(run_artifacts_dir)?;
    let run_id = run_id.to_owned();
    await_blocking("run_transcript.native", move || {
        Ok::<_, String>(persist_session_record_to(&dir, &run_id, &record))
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal but representative `stream-json` NDJSON stream: system init, an assistant turn with
    // text + a tool_use, the user turn carrying the tool_result, then the final result event.
    const STREAM_JSON: &str = r#"{"type":"system","subtype":"init","session_id":"s1"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Reading the file."},{"type":"tool_use","id":"tu_1","name":"Read","input":{"path":"auth.py"}}],"usage":{"input_tokens":12,"output_tokens":34}}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_1","content":"file contents here"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done."}],"usage":{"output_tokens":5}}}
{"type":"result","subtype":"success","result":"Fixed the bug in auth.py","usage":{"input_tokens":12,"output_tokens":39}}"#;

    #[test]
    fn cx4_native_persistence_redacts_both_structured_and_bounded_text_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
        let record=codesense_pty_core::agent_cli::output::SessionRecord {
            session_id:"01a15600-0001-7000-8000-000000000002".into(),
            format:codesense_pty_core::agent_cli::adapter(codesense_pty_core::agent_cli::AgentCli::Codex).output().transcript.unwrap().interactive.as_ref().unwrap().format,
            raw:format!("{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"{secret}\"}}]}}}}\n"),
        };
        assert_eq!(
            persist_session_record_to(dir.path(), "run-native", &record).as_deref(),
            Some("run-artifacts/run-native.turns.json")
        );
        let persisted = std::fs::read_to_string(dir.path().join("run-native.turns.json")).unwrap();
        assert!(!persisted.contains(secret));
        assert!(persisted.contains("[redacted]"));
        assert!(persisted.contains(&record.session_id));
        assert!(
            !dir.path().join("run-native.raw.log").exists(),
            "structured consent does not grant raw terminal capture"
        );
        let fallback = codesense_pty_core::agent_cli::output::SessionRecord {
            raw: format!("{} {secret} TAIL", "x".repeat(2 * 1024 * 1024)),
            ..record
        };
        assert_eq!(
            persist_session_record_to(dir.path(), "run-fallback", &fallback).as_deref(),
            Some("run-artifacts/run-fallback.log")
        );
        let persisted = std::fs::read_to_string(dir.path().join("run-fallback.log")).unwrap();
        assert!(!persisted.contains(secret));
        assert!(persisted.contains("[redacted]"));
        assert!(persisted.ends_with("TAIL"));
        assert!(persisted.len() <= 1024 * 1024);
    }

    #[test]
    fn stream_json_maps_to_neutral_turns_with_tool_use_result_and_usage() {
        // AC-2: turns / tool_use / tool_result / usage from the headless stream.
        let t = parse_transcript(TranscriptFormat::AnthropicStreamJson, STREAM_JSON)
            .expect("parses to turns");
        assert_eq!(t.turns.len(), 2, "two assistant turns");
        let first = &t.turns[0];
        assert_eq!(first.role, "assistant");
        assert_eq!(first.text.as_deref(), Some("Reading the file."));
        assert_eq!(first.tool_calls.len(), 1);
        let call = &first.tool_calls[0];
        assert_eq!(call.name, "Read");
        assert!(call.input.as_deref().unwrap().contains("auth.py"));
        assert_eq!(call.result.as_deref(), Some("file contents here"));
        assert_eq!(call.is_error, Some(false));
        let usage = first.usage.as_ref().expect("per-turn usage");
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(34));
    }

    #[test]
    fn session_jsonl_yields_the_same_neutral_shape() {
        // AC-2: the on-disk session JSONL (message-wrapped) maps identically.
        const SESSION_JSONL: &str = r#"{"type":"summary","summary":"x"}
{"type":"assistant","uuid":"a1","message":{"role":"assistant","content":[{"type":"text","text":"Hi"},{"type":"tool_use","id":"tu_9","name":"Bash","input":{"cmd":"ls"}}]}}
{"type":"user","uuid":"u1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_9","content":[{"type":"text","text":"a\nb"}],"is_error":true}]}}"#;
        let t = parse_transcript(TranscriptFormat::AnthropicSessionJsonl, SESSION_JSONL)
            .expect("parses");
        assert_eq!(t.turns.len(), 1);
        let call = &t.turns[0].tool_calls[0];
        assert_eq!(call.name, "Bash");
        assert_eq!(call.result.as_deref(), Some("a\nb"));
        assert_eq!(
            call.is_error,
            Some(true),
            "tool_result is_error flows through"
        );
    }

    const CLAUDE_SESSION_2_1_296: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/codesense-pty-core/tests/fixtures/claude/session-2.1.296.jsonl"
    ));

    #[test]
    fn a_real_shaped_claude_session_keeps_the_persons_prompts_but_not_tool_result_rows() {
        let t = parse_transcript(
            TranscriptFormat::AnthropicSessionJsonl,
            CLAUDE_SESSION_2_1_296,
        )
        .expect("parses");
        let roles: Vec<&str> = t.turns.iter().map(|t| t.role.as_str()).collect();
        assert_eq!(
            roles,
            ["user", "assistant", "assistant", "assistant", "user"]
        );
        assert_eq!(
            t.turns[0].text.as_deref(),
            Some("Please read auth.py and fix the bug")
        );
        assert_eq!(t.turns[4].text.as_deref(), Some("/bmad-help what next?"));
        let calls = &t.turns[2].tool_calls;
        assert_eq!(calls[0].name, "Skill");
        assert_eq!(calls[0].is_error, Some(false));
        assert_eq!(calls[1].name, "mcp__codesense__read_messages");
        assert_eq!(calls[1].is_error, Some(true), "is_error flows through");
        assert_eq!(t.turns[1].tool_calls[0].is_error, Some(false));
        assert!(t.unavailable.contains_key("turn.cost"));
        // Claude's skill tool names the skill; no other call does.
        assert_eq!(calls[0].skill.as_deref(), Some("bmad-help"));
        assert_eq!(calls[1].skill, None);
        assert_eq!(t.turns[1].tool_calls[0].skill, None);
    }

    #[test]
    fn a_claude_call_with_no_result_yet_has_no_error_verdict() {
        let raw = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#;
        let t = parse_transcript(TranscriptFormat::AnthropicSessionJsonl, raw).unwrap();
        assert_eq!(t.turns[0].tool_calls[0].is_error, None);
    }

    #[test]
    fn meta_sidechain_and_non_human_user_rows_are_not_the_persons_request() {
        let raw = [
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"caveat"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"sub-agent prompt"}}"#,
            r#"{"type":"user","origin":{"kind":"task-notification"},"message":{"role":"user","content":"bg done"}}"#,
            r#"{"type":"user","origin":{"kind":"human"},"message":{"role":"user","content":"real"}}"#,
        ]
        .join("\n");
        let t = parse_transcript(TranscriptFormat::AnthropicSessionJsonl, &raw).unwrap();
        assert_eq!(t.turns.len(), 1);
        assert_eq!(t.turns[0].text.as_deref(), Some("real"));
    }

    #[test]
    fn claude_turns_carry_the_neutral_fields_the_session_record_holds() {
        let t = parse_transcript(
            TranscriptFormat::AnthropicSessionJsonl,
            CLAUDE_SESSION_2_1_296,
        )
        .expect("parses");
        let indexes: Vec<_> = t.turns.iter().map(|t| t.index).collect();
        assert_eq!(indexes, [Some(0), Some(1), Some(2), Some(3), Some(4)]);
        let first = &t.turns[1];
        assert_eq!(first.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(
            first
                .reasoning
                .as_ref()
                .map(|r| (r.kind, r.text.as_deref())),
            Some((ReasoningKind::None, None)),
            "the thinking block is empty with a signature"
        );
        assert!(first.ts_ms.is_some() && t.turns[0].ts_ms < first.ts_ms);
        let read = &first.tool_calls[0];
        assert_eq!(read.native_id.as_deref(), Some("toolu_01AAA"));
        assert_eq!(read.mcp_server, None);
        assert!(read.ts_ms.is_some());
        let mcp = &t.turns[2].tool_calls[1];
        assert_eq!(mcp.mcp_server.as_deref(), Some("codesense"));
        assert_eq!(mcp.native_id.as_deref(), Some("toolu_01CCC"));
    }

    #[test]
    fn a_turns_file_from_before_the_neutral_fields_still_reads_and_round_trips() {
        const LEGACY: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/crates/codesense-pty-core/tests/fixtures/turns/legacy-run.turns.json"
        ));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("run-legacy.turns.json"), LEGACY).unwrap();
        let t = read_turns_from(dir.path(), "run-legacy").expect("legacy file reads");
        assert_eq!(t.session_id.as_deref(), Some("s-legacy"));
        assert_eq!(t.turns.len(), 2);
        let calls = &t.turns[0].tool_calls;
        assert_eq!(calls[0].is_error, Some(true), "a stored failure stays one");
        assert_eq!(
            calls[1].is_error, None,
            "an absent flag is not claimed as success"
        );
        assert_eq!(t.turns[0].index, None);
        assert_eq!(t.turns[0].model, None);
        assert!(t.unavailable.is_empty());
        let again: SessionTranscript =
            serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(again, t);
    }

    fn stored_for(dir: &Path, session: &str, text: &str) {
        let stored = crate::session_conversation::StoredConversation {
            session_id: session.into(),
            source_format: "anthropic-session-jsonl-v1".into(),
            captured_at_ms: 1,
            truncated: None,
            transcript: SessionTranscript::new(
                None,
                vec![Turn {
                    role: "assistant".into(),
                    text: Some(text.into()),
                    ..Turn::default()
                }],
                Default::default(),
            ),
        };
        let file = crate::session_conversation::conversation_file(dir, session).unwrap();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(file, serde_json::to_vec(&stored).unwrap()).unwrap();
    }

    #[test]
    fn a_run_reads_the_conversation_of_its_latest_session_that_has_one() {
        let runs = tempfile::tempdir().unwrap();
        let convs = tempfile::tempdir().unwrap();
        stored_for(convs.path(), "tab-1", "first session");
        stored_for(convs.path(), "tab-2", "second session");
        let read = |sessions: &[&str]| {
            let sessions: Vec<String> = sessions.iter().map(|s| s.to_string()).collect();
            read_transcript_for_run_from(Some(runs.path()), Some(convs.path()), "run-1", &sessions)
                .and_then(|t| t.turns[0].text.clone())
        };
        assert_eq!(read(&["tab-1", "tab-2"]).as_deref(), Some("second session"));
        assert_eq!(read(&["tab-1", "tab-9"]).as_deref(), Some("first session"));
        assert_eq!(
            read(&["tab-9"]),
            None,
            "no stored session and no legacy file"
        );
        assert_eq!(read(&[]), None);
    }

    #[test]
    fn an_older_run_still_reads_its_own_turns_file_when_no_session_has_a_conversation() {
        let runs = tempfile::tempdir().unwrap();
        let convs = tempfile::tempdir().unwrap();
        std::fs::write(
            runs.path().join("run-old.turns.json"),
            r#"{"turns":[{"role":"assistant","text":"legacy turn"}]}"#,
        )
        .unwrap();
        let none: Vec<String> = Vec::new();
        let got =
            read_transcript_for_run_from(Some(runs.path()), Some(convs.path()), "run-old", &none)
                .unwrap();
        assert_eq!(got.turns[0].text.as_deref(), Some("legacy turn"));
        // A session conversation wins over the legacy file when the run names one.
        stored_for(convs.path(), "tab-1", "stored turn");
        let got = read_transcript_for_run_from(
            Some(runs.path()),
            Some(convs.path()),
            "run-old",
            &["tab-1".to_owned()],
        )
        .unwrap();
        assert_eq!(got.turns[0].text.as_deref(), Some("stored turn"));
    }

    #[test]
    fn a_secret_in_a_stored_conversation_is_masked_when_a_run_reads_it() {
        let convs = tempfile::tempdir().unwrap();
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123";
        stored_for(convs.path(), "tab-1", &format!("leaked {token}"));
        let got =
            read_transcript_for_run_from(None, Some(convs.path()), "run-1", &["tab-1".to_owned()])
                .unwrap();
        assert!(!got.turns[0].text.as_deref().unwrap().contains(token));
    }

    #[test]
    fn a_headless_run_keeps_a_turns_file_only_for_a_format_with_no_session_log() {
        for (format, written) in [
            (TranscriptFormat::AnthropicStreamJson, false),
            (TranscriptFormat::AnthropicSessionJsonl, false),
            (TranscriptFormat::CodexSessionJsonl, false),
            (TranscriptFormat::OpenCodeRunJson, true),
            (TranscriptFormat::HermesStreamJson, true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let stdout = match format {
                TranscriptFormat::OpenCodeRunJson => {
                    r#"{"type":"text","timestamp":1,"sessionID":"s","part":{"text":"hello"}}"#
                }
                TranscriptFormat::HermesStreamJson => {
                    crate::routines::test_support::HERMES_DENY_RUN
                }
                _ => STREAM_JSON,
            };
            persist_structured_to(dir.path(), "run-h", format, stdout.as_bytes(), b"");
            assert_eq!(
                dir.path().join("run-h.turns.json").exists(),
                written,
                "{format:?}"
            );
            assert!(
                dir.path().join("run-h.log").exists(),
                "the text floor stays for {format:?}"
            );
        }
    }

    #[test]
    fn a_hermes_run_reads_back_its_prose_paired_tool_result_and_native_identity() {
        let dir = tempfile::tempdir().unwrap();
        let raw = crate::routines::test_support::HERMES_DENY_RUN;
        persist_structured_to(dir.path(), "run-hm", TranscriptFormat::HermesStreamJson, raw.as_bytes(), b"");
        let t = read_transcript_for_run_from(Some(dir.path()), None, "run-hm", &[]).unwrap();
        assert!(t.session_id.as_deref().is_some_and(|id| id.starts_with("local-")));
        let call = &t.turns[0].tool_calls[0];
        assert_eq!((call.name.as_str(), call.is_error), ("terminal", Some(true)));
        assert!(call.result.as_deref().unwrap().contains("BLOCKED"));
        assert_eq!(t.turns[1].text.as_deref(), Some("PRB_FINISHED"));
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        // AC-5: a garbage / version-bumped line is skipped; the good turns still parse.
        let raw = format!("not json at all\n{{\"type\":\"weird\"}}\n{STREAM_JSON}");
        let t = parse_transcript(TranscriptFormat::AnthropicStreamJson, &raw)
            .expect("good lines still parse");
        assert_eq!(t.turns.len(), 2);
    }

    #[test]
    fn wholly_unparseable_source_returns_none() {
        // AC-5: no turns extractable → None (drop to the text floor, no crash).
        assert!(parse_transcript(
            TranscriptFormat::AnthropicStreamJson,
            "garbage\nmore garbage"
        )
        .is_none());
        assert!(parse_transcript(TranscriptFormat::AnthropicStreamJson, "").is_none());
    }

    #[test]
    fn opencode_summary_is_the_final_stop_text_or_the_error_message() {
        use crate::routines::test_support::{OPENCODE_ERROR_RUN, OPENCODE_TEXT_RUN};
        let format = TranscriptFormat::OpenCodeRunJson;
        assert_eq!(
            result_summary(format, OPENCODE_TEXT_RUN).as_deref(),
            Some("FINAL-ANSWER: the fake model says hello.")
        );
        assert_eq!(run_failure(format, OPENCODE_TEXT_RUN), None);
        assert_eq!(
            result_summary(format, OPENCODE_ERROR_RUN).as_deref(),
            Some("fake bad request")
        );
        assert_eq!(
            run_failure(format, OPENCODE_ERROR_RUN).as_deref(),
            Some("fake bad request")
        );
        assert_eq!(result_summary(format, "not json"), None);
        // The Anthropic formats carry no failure of their own beyond the exit.
        assert_eq!(
            run_failure(TranscriptFormat::AnthropicStreamJson, OPENCODE_ERROR_RUN),
            None
        );
    }

    #[test]
    fn opencode_stdout_parses_through_the_format_seam_and_persists_redacted() {
        use crate::routines::test_support::OPENCODE_TEXT_RUN;
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
        let raw = OPENCODE_TEXT_RUN.replace("the fake model says hello", secret);
        let dir = tempfile::tempdir().unwrap();
        persist_structured_to(
            dir.path(),
            "run-oc",
            TranscriptFormat::OpenCodeRunJson,
            raw.as_bytes(),
            b"",
        );
        let turns = std::fs::read_to_string(dir.path().join("run-oc.turns.json")).unwrap();
        let log = std::fs::read_to_string(dir.path().join("run-oc.log")).unwrap();
        for written in [&turns, &log] {
            assert!(!written.contains(secret), "{written}");
        }
        assert!(turns.contains("ses_e8efa55b0ffeb98GMs6Q3lPTHm"));
        assert!(turns.contains("FINAL-ANSWER"));
    }

    #[test]
    fn result_summary_prefers_the_result_event_over_raw_text() {
        // AC-6: summary comes from the `result` event, not a clamp of raw JSON.
        assert_eq!(
            result_summary(TranscriptFormat::AnthropicStreamJson, STREAM_JSON).as_deref(),
            Some("Fixed the bug in auth.py")
        );
        // No result event → None, so the caller keeps today's text clamp.
        assert!(result_summary(
            TranscriptFormat::AnthropicStreamJson,
            "{\"type\":\"assistant\"}"
        )
        .is_none());
        // Non-stream format has no result-event contract.
        assert!(result_summary(TranscriptFormat::AnthropicSessionJsonl, STREAM_JSON).is_none());
    }

    #[test]
    fn read_turns_scrubs_secrets_at_render_across_text_and_tool_fields() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
        // A turns file written raw (older evidence, or hand-edited) — bypasses the persist scrub.
        let transcript = SessionTranscript {
            session_id: None,
            turns: vec![Turn {
                role: "assistant".into(),
                text: Some(format!("here is {secret}")),
                tool_calls: vec![ToolCall {
                    name: "sh".into(),
                    input: Some(format!("export TOKEN={secret}")),
                    result: Some(format!("ok {secret}")),
                    ..ToolCall::default()
                }],
                ..Turn::default()
            }],
            unavailable: Default::default(),
        };
        std::fs::write(
            dir.path().join("run-x.turns.json"),
            serde_json::to_string(&transcript).unwrap(),
        )
        .unwrap();
        let got = read_turns_from(dir.path(), "run-x").expect("reads the turns file");
        let rendered = serde_json::to_string(&got).unwrap();
        assert!(
            !rendered.contains(secret),
            "no secret survives the render read"
        );
        assert!(
            rendered.contains("[redacted]"),
            "render scrub leaves a marker"
        );
    }

    #[test]
    fn a_retry_rotates_the_prior_turns_and_never_clobbers_it() {
        let dir = tempfile::tempdir().unwrap();
        let first = SessionTranscript {
            session_id: None,
            turns: vec![Turn {
                role: "assistant".into(),
                text: Some("attempt one".into()),
                ..Turn::default()
            }],
            unavailable: Default::default(),
        };
        let second = SessionTranscript {
            session_id: None,
            turns: vec![Turn {
                role: "assistant".into(),
                text: Some("attempt two".into()),
                ..Turn::default()
            }],
            unavailable: Default::default(),
        };
        write_turns_to(dir.path(), "run-1", &first).unwrap();
        write_turns_to(dir.path(), "run-1", &second).unwrap();
        // Legacy name holds the latest; the prior attempt rotates to a1 (AC-3b, .turns.json kind).
        assert_eq!(
            read_turns_from(dir.path(), "run-1").as_ref(),
            Some(&second),
            "legacy name holds the latest attempt"
        );
        let archived: SessionTranscript = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("run-1.a1.turns.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(archived, first, "prior turns rotated, never clobbered");
    }

    #[test]
    fn turns_round_trip_through_persisted_json() {
        let dir = tempfile::tempdir().unwrap();
        let t = parse_transcript(TranscriptFormat::AnthropicStreamJson, STREAM_JSON).unwrap();
        let reff = write_turns_to(dir.path(), "run-abc", &t).unwrap();
        assert_eq!(reff, "run-artifacts/run-abc.turns.json");
        assert_eq!(read_turns_from(dir.path(), "run-abc").as_ref(), Some(&t));
    }

    #[test]
    fn turns_filename_rejects_traversal() {
        assert_eq!(turns_filename("../escape"), None);
        assert_eq!(turns_filename("a/b"), None);
        assert_eq!(
            turns_filename("run-headless-123-4").as_deref(),
            Some("run-headless-123-4.turns.json")
        );
    }

    #[test]
    fn transcript_inventory_accepts_only_exact_managed_turns_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "run-interactive.turns.json",
            "run-headless.turns.json",
            "run-headless.log",
            "run-temp.turns.json.tmp.1",
            ".turns.json",
            "nested/run-nested.turns.json",
        ] {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, "{}").unwrap();
        }

        let ids = materialized_transcript_run_ids_in(dir.path());
        assert_eq!(
            ids,
            ["run-headless".to_string(), "run-interactive".to_string()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn transcript_inventory_degrades_to_empty_for_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(materialized_transcript_run_ids_in(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn seeded_secret_never_reaches_the_persisted_log_or_turns() {
        // AC-4 end-to-end: a credential echoed in a tool_result must be scrubbed from BOTH the `.log`
        // text floor AND `{run_id}.turns.json` before either is written — proved at the persist seam,
        // not just the pure redactor.
        let dir = tempfile::tempdir().unwrap();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
        let stream = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"reading"}},{{"type":"tool_use","id":"tu_1","name":"Bash","input":{{"cmd":"echo {secret}"}}}}]}}}}
{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"tu_1","content":"leaked {secret} here"}}]}}}}"#
        );
        let reff = persist_structured_to(
            dir.path(),
            "run-sec",
            TranscriptFormat::AnthropicStreamJson,
            stream.as_bytes(),
            b"",
        )
        .expect("persist writes the .log floor");
        assert_eq!(reff, "run-artifacts/run-sec.log");

        // Neither on-disk artifact may contain the raw token.
        let log = std::fs::read_to_string(dir.path().join("run-sec.log")).unwrap();
        assert!(!log.contains(secret), "secret leaked to .log: {log}");
        // The turns are stored by session now (and scrubbed there); no per-run turns file is made.
        assert!(!dir.path().join("run-sec.turns.json").exists());
    }

    // ── Interactive session-log resolver (SW-OR-15) ─────────────────────────────────────────────

    #[test]
    fn duplicate_tool_use_id_keeps_the_first_calls_result() {
        // A repeated id must not let a later tool_use clobber the mapping and steal the first call's
        // result (first-wins via `or_insert`).
        const DUP: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu_x","name":"First","input":{}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_x","content":"first result"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu_x","name":"Second","input":{}}]}}"#;
        let t = parse_transcript(TranscriptFormat::AnthropicStreamJson, DUP).unwrap();
        assert_eq!(t.turns[0].tool_calls[0].name, "First");
        assert_eq!(
            t.turns[0].tool_calls[0].result.as_deref(),
            Some("first result"),
            "the first call keeps its result"
        );
        assert_eq!(t.turns[1].tool_calls[0].name, "Second");
        assert!(t.turns[1].tool_calls[0].result.is_none());
    }
}
