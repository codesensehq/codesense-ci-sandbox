// SPDX-License-Identifier: MIT
/**
 * SW-OR-16 — the FE mirror of the trusted-side parsed structured transcript
 * (`run_transcript.rs::SessionTranscript`). The panel reads ALREADY-PARSED, already-redacted,
 * already-bounded turns via `readRunTranscript(run_id)` — no NDJSON/JSONL ever touches the render
 * path. Absent (`null`) → the panel stays on SW-OR-12's plain-text body (graceful text fallback).
 *
 * Fields are optional to match the Rust `skip_serializing_if` serialization (an omitted `text`,
 * empty `tool_calls`, or absent `usage` simply does not appear in the JSON).
 */

/** One neutral turn: a speaker, its prose, its tool activity, and its per-turn token usage. */
export interface TranscriptTurn {
  /** Speaker — `"assistant"` / `"user"` / `"result"`. */
  role: string;
  /** Joined prose blocks for the turn, when any. */
  text?: string | null;
  /** Tool invocations in this turn (a `tool_use` paired with its later `tool_result`). */
  tool_calls?: ToolCall[];
  /** Per-turn token usage, when the source reported it. */
  usage?: TurnUsage | null;
}

/** A neutral tool invocation: the call (`name` + rendered `input`) paired with its result. */
export interface ToolCall {
  /** The tool name (e.g. `Read`, `Bash`). */
  name: string;
  /** The rendered tool input (compact JSON / bare string), when present. */
  input?: string | null;
  /** The tool result text, when the matching `tool_result` arrived. */
  result?: string | null;
  /** True when the tool reported an error result. */
  is_error?: boolean;
}

/** Per-turn token usage (neutral subset). */
export interface TurnUsage {
  input_tokens?: number | null;
  output_tokens?: number | null;
}

/** The parsed structured transcript — the optional turn-by-turn upgrade on top of the text floor. */
export interface SessionTranscript {
  /** Captured native thread id, when its source provides one. */
  session_id?: string | null;
  turns: TranscriptTurn[];
}
