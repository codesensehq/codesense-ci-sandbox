// SPDX-License-Identifier: MIT
//! Shared transcript values; native record readers and parsers belong to their CLI adapter.
use super::TranscriptFormat;
use serde::{Deserialize, Serialize};

/// Bound native reads, transport and parsing independently of native log size.
pub const MAX_RECORD_BYTES: usize = 1024 * 1024;

/// The last [`MAX_RECORD_BYTES`] of `raw`, cut on a character boundary. A line the cut splits is
/// left for the line reader to skip.
pub(super) fn tail(raw: &str) -> &str {
    let mut start = raw.len().saturating_sub(MAX_RECORD_BYTES);
    while !raw.is_char_boundary(start) {
        start += 1;
    }
    &raw[start..]
}

/// The JSON events of the tail of a run's stdout, one per line: a line that is not JSON
/// (including one the bound cut) is skipped, so a changed stream degrades, never fails.
pub(super) fn json_events(raw: &str) -> impl Iterator<Item = serde_json::Value> + '_ {
    tail(raw)
        .lines()
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
}

/// A bounded native session record, selected by its captured provider session id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub format: TranscriptFormat,
    pub raw: String,
}

/// The parsed, agent-neutral transcript persisted to `{run_id}.turns.json` and served to the panel.
/// A thin wrapper (not a bare `Vec`) so a future metadata field is additive. `body` stays in the
/// run's `.log` (single-sourced), so this carries ONLY the structured upgrade.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionTranscript {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub turns: Vec<Turn>,
    /// `{field: reason}` for each fact this CLI's record does not hold. Nothing is inferred to
    /// fill a gap; a reader shows what exists and skips what is listed here.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unavailable: std::collections::BTreeMap<String, String>,
}

impl SessionTranscript {
    /// The transcript of `turns`, numbered in order.
    pub fn new(
        session_id: Option<String>,
        mut turns: Vec<Turn>,
        unavailable: std::collections::BTreeMap<String, String>,
    ) -> Self {
        for (index, turn) in turns.iter_mut().enumerate() {
            turn.index = u32::try_from(index).ok();
        }
        Self {
            session_id,
            turns,
            unavailable,
        }
    }
}

/// One neutral turn. `role` is the speaker (`user` / `assistant` / `result`); `text` the joined prose
/// blocks; `tool_calls` the tool activity (a `tool_use` paired with its later `tool_result`); `usage`
/// the per-turn token counts when the source reports them. Every other field is filled only when the
/// CLI's record holds it; a fact a CLI never records is named in `unavailable` (per turn) or in
/// [`SessionTranscript::unavailable`] (for every turn), never guessed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Position in the whole conversation, from 0. It stays stable when a size limit drops turns.
    /// `None` for a turns file written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// When the CLI wrote the turn, epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<i64>,
    /// The model that produced an assistant turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    /// `{field: reason}` for a fact this turn lacks that the CLI could have recorded.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unavailable: std::collections::BTreeMap<String, String>,
}

/// How much of the model's reasoning a CLI's record holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningKind {
    /// The reasoning text as the model wrote it.
    Full,
    /// A summary the CLI or provider wrote of it.
    Summary,
    /// Present but unreadable (an encrypted blob).
    Encrypted,
    /// The record holds a reasoning step but no text of it.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reasoning {
    pub kind: ReasoningKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A neutral tool invocation: the `tool_use` (`name` + rendered `input`) paired with its
/// `tool_result` (`result` + `is_error`) once the matching result arrives.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Whether the call failed: `None` when the CLI's record does not say (see
    /// [`SessionTranscript::unavailable`]), never guessed from the output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// The CLI's own id for the call (Claude `toolu_…`, Codex `call_id`, OpenCode `callID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_id: Option<String>,
    /// Other ids the CLI gives this call's work (Codex's `exec-<uuid>` file-change item ids),
    /// each resolving to this call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// The MCP server a `mcp__<server>__<tool>` call went to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<i64>,
    /// The skill this call invoked (Claude's `Skill` tool, OpenCode's `skill` tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
}

impl ToolCall {
    /// A call as the CLI named it, with its server worked out from the name.
    pub fn named(name: impl Into<String>) -> Self {
        let name = name.into();
        let mcp_server = mcp_server_of(&name).map(str::to_owned);
        Self {
            name,
            mcp_server,
            ..Self::default()
        }
    }
}

/// The server of a `mcp__<server>__<tool>` tool name, or of a bare `mcp__<server>__` namespace.
pub fn mcp_server_of(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("mcp__")?;
    let server = rest.split_once("__").map_or(rest, |(server, _)| server);
    (!server.is_empty()).then_some(server)
}

/// Epoch milliseconds of an RFC 3339 timestamp as a CLI writes it.
pub fn rfc3339_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// Per-turn token usage (neutral subset). Fields optional so a source that omits one still parses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

impl Usage {
    pub fn from_value(v: &serde_json::Value) -> Option<Usage> {
        let usage = v.get("usage")?;
        let input_tokens = usage
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64);
        if input_tokens.is_none() && output_tokens.is_none() {
            return None;
        }
        Some(Usage {
            input_tokens,
            output_tokens,
        })
    }
}
