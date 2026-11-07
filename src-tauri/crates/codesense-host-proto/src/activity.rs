// SPDX-License-Identifier: MIT
//! Bounded, content-free activity observation wire types.
use crate::agent_cli::AgentCli;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Idle,
    Thinking,
    Working,
    NeedsPermission,
    Done,
    Errored,
    Crashed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Event {
    Started,
    Submitted,
    ToolStarted,
    ToolFinished,
    Waiting,
    Stopped,
    Failed,
    Interrupted,
    Ended,
    /// A subagent started, or resumed: the report's [`agent`](Report::agent).
    SubagentStarted,
    /// A subagent finished: the report's [`agent`](Report::agent).
    SubagentFinished,
    /// The provider compacts its context inside a turn.
    Compacting,
    /// The provider sits at its prompt, waiting for the person.
    Prompting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Main,
    Child,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    pub provider: AgentCli,
    pub event: Event,
    pub scope: Scope,
    pub report_id: String,
    pub provider_session: Option<String>,
    pub turn: Option<String>,
    pub tool: Option<String>,
    /// The tool's name (`Bash`, `WebFetch`, an MCP tool's name), never its input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// An allow-listed, content-free cause: an error kind for `Failed`, or
    /// `question` for a wait on an answer rather than a permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The subagent the report is about or came from, by the CLI's own opaque id. A
    /// report with one is never the main agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// At the main turn's end, the subagents the CLI itself says still run, when it
    /// says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<Vec<String>>,
}

/// The most subagents a report may list: their ids at the opaque limit still fit the
/// activity socket's 16 KiB frame.
pub const MAX_SUBAGENTS: usize = 64;

/// The causes a report may carry. Anything else is never admitted.
pub const REASONS: &[&str] = &[
    "question",
    "rate_limit",
    "overloaded",
    "authentication_failed",
    "oauth_org_not_allowed",
    "account_on_hold",
    "billing_error",
    "invalid_request",
    "model_not_found",
    "server_error",
    "max_output_tokens",
    "cloud_credential_error",
    "unknown",
];

/// Whether `value` is an id a report may carry: short, non-empty, no control characters.
pub fn opaque(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

impl Report {
    pub fn valid(&self) -> bool {
        opaque(&self.report_id)
            && self
                .reason
                .as_deref()
                .is_none_or(|reason| REASONS.contains(&reason))
            && [
                &self.provider_session,
                &self.turn,
                &self.tool,
                &self.tool_name,
                &self.agent,
            ]
            .into_iter()
            .flatten()
            .all(|value| opaque(value))
            && self.subagents.as_ref().is_none_or(|subagents| {
                subagents.len() <= MAX_SUBAGENTS && subagents.iter().all(|id| opaque(id))
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub host_boot_id: String,
    pub session_incarnation: String,
    pub invocation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub session: String,
    pub identity: Identity,
    pub revision: u64,
    pub turn_generation: u64,
    pub provider: AgentCli,
    pub state: State,
    /// Provider evidence was observed; this is not a claim of full lifecycle coverage.
    pub observed: bool,
    pub limited: bool,
    pub exit: Option<Exit>,
    pub asks: Vec<String>,
    /// Why the current state holds, when the provider said: an error kind while
    /// `errored`, `question` while waiting on an answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Positive host tool tracking, absent on older or unobserved owners.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_open: Option<bool>,
    /// Elapsed milliseconds since actual provider evidence or PTY output,
    /// measured by the host's monotonic clock when this snapshot is captured.
    /// Absent means no activity timestamp is available, never "just active".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_age_ms: Option<u64>,
    /// The subagents of its agent that run now. `state` stays the main agent's own.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub subagents_open: u32,
}

impl Snapshot {
    /// Whether its agent is seen at its prompt: observed, and idle or done.
    pub fn at_prompt(&self) -> bool {
        self.observed && matches!(self.state, State::Idle | State::Done)
    }

    /// Whether it may be handed input now: at its prompt, still running, asking nothing.
    pub fn takes_input(&self) -> bool {
        self.at_prompt() && self.exit.is_none() && self.asks.is_empty()
    }
}

fn is_zero(count: &u32) -> bool {
    *count == 0
}

/// Control-plane observations carry the scope captured when the operation
/// started, so a delayed resolution cannot target a newer turn or invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mutation {
    pub session: String,
    pub identity: Identity,
    pub turn_generation: u64,
    pub operation_id: String,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    Progress { blocked: bool },
    Done { outcome: Outcome },
    AskOpened { id: String },
    AskClosed { id: String, resolved: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Success,
    Failed,
    NeedsAttention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exit {
    pub code: Option<u32>,
    pub signal: Option<String>,
    pub requested: bool,
}

/// Returned only to the admitted launch controller. Never included in snapshots.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Writer {
    pub identity: Identity,
    pub token: String,
    pub socket: String,
}

/// Public launch material; the capability stays in a host-private file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellLaunch {
    pub identity: Identity,
    pub helper: String,
    pub bootstrap: String,
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityWriter")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::Snapshot;

    #[test]
    fn snapshot_activity_metadata_is_additive_and_missing_fields_stay_unknown() {
        let old = serde_json::json!({
            "session": "s",
            "identity": {"hostBootId": "boot", "sessionIncarnation": "pty", "invocationId": "run"},
            "revision": 1, "turnGeneration": 1, "provider": "claude",
            "state": "working", "observed": true, "limited": false,
            "exit": null, "asks": []
        });
        let mut snapshot: Snapshot = serde_json::from_value(old).unwrap();
        assert_eq!(snapshot.tool_open, None);
        assert_eq!(snapshot.activity_age_ms, None);
        let old_roundtrip = serde_json::to_value(&snapshot).unwrap();
        assert!(old_roundtrip.get("toolOpen").is_none());
        assert!(old_roundtrip.get("activityAgeMs").is_none());
        assert_eq!(snapshot.subagents_open, 0);
        assert!(old_roundtrip.get("subagentsOpen").is_none());
        for tool_open in [false, true] {
            snapshot.tool_open = Some(tool_open);
            snapshot.activity_age_ms = Some(600_000);
            snapshot.subagents_open = 2;
            let wire = serde_json::to_value(&snapshot).unwrap();
            assert_eq!(wire["toolOpen"], tool_open);
            assert_eq!(wire["activityAgeMs"], 600_000);
            assert_eq!(wire["subagentsOpen"], 2);
            assert_eq!(serde_json::from_value::<Snapshot>(wire).unwrap(), snapshot);
        }
    }
}
