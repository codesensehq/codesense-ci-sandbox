// SPDX-License-Identifier: MIT
//! The MCP tools that read and manage This Mac's terminal server for
//! this instance: the same data and mechanisms as Settings → General. The
//! tool bodies live here, over a given app handle, sink, end and launch, so
//! they are tested against a real terminal server; `handler.rs` only wires
//! them to the app's own.

use std::path::Path;
use std::sync::Arc;

use codesense_pty_core::domain::TerminalEventSink;
use rmcp::schemars::JsonSchema;
use serde::Deserialize;
use tauri::{AppHandle, Runtime};

use crate::commands::pty_backend::{self, RestartError, TerminalServerOverview, TerminalServerRun};
use crate::terminal::session_server::LocalActivityUpgrade;

/// What a stop or restart binds to, exactly as `terminal_server_status`
/// returned it in `expected`: the server, its running sessions, and the
/// other CodeSense instances connected to it (`null` when it cannot say).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalServerExpected {
    pub server_id: String,
    pub epoch: String,
    pub pid: u32,
    pub session_ids: Vec<String>,
    pub other_instances: Option<u32>,
}

impl From<TerminalServerExpected> for LocalActivityUpgrade {
    fn from(expected: TerminalServerExpected) -> Self {
        Self {
            server_id: expected.server_id,
            epoch: expected.epoch,
            pid: expected.pid,
            session_ids: expected.session_ids,
        }
    }
}

/// The incompatible server reviewed before authorizing a restart. Unknown
/// values stay unknown; learning a count after review is also a change.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewedIncompatible {
    pub pid: Option<u32>,
    pub sessions: Option<u32>,
    pub other_instances: Option<u32>,
}

impl ReviewedIncompatible {
    pub(crate) fn from_overview(overview: &TerminalServerOverview) -> Self {
        Self {
            pid: overview.pid,
            sessions: overview.ends,
            other_instances: overview.other_instances,
        }
    }

    fn check(&self, now: &TerminalServerOverview) -> Result<(), String> {
        if now.state == TerminalServerRun::Incompatible && *self == Self::from_overview(now) {
            return Ok(());
        }
        Err(format!(
            "the terminal server changed since you read the status (now {}; it {}): nothing ended — read terminal_server_status again",
            serde_json::json!(now),
            ends_line(now)
        ))
    }
}

/// `stop_terminal_server` and `restart_terminal_server`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndTerminalServerParams {
    /// Explicit authorization to end every terminal and agent the server runs.
    pub confirm_end_sessions: bool,
    /// `expected` from `terminal_server_status`. Required for a running
    /// server; omitted only to restart an incompatible one.
    #[serde(default)]
    pub expected: Option<TerminalServerExpected>,
    /// For an incompatible restart, pass `{pid, sessions, otherInstances}`
    /// from the reviewed status (`sessions` is `incompatible.liveSessions`).
    /// Optional for compatibility; passing it guards against a change.
    #[serde(default)]
    pub reviewed_incompatible: Option<ReviewedIncompatible>,
}

/// `end_unowned_terminal_session`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndUnownedSessionParams {
    /// A session listed in `unowned` by `terminal_server_status`.
    pub session: String,
    /// Explicit authorization to end it, and whatever it runs.
    pub confirm_end_session: bool,
}

fn plural(n: u32, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// What ending the server ends, in words, for a confirmation and a tool
/// result: the running sessions it ends (an unknown count is said to be
/// unknown, never 0), the running agents among them, and the other app
/// instances whose tabs end too — or that whether there are any cannot be
/// told (a server older than the question).
pub(crate) fn ends_line(overview: &TerminalServerOverview) -> String {
    let mut line = match overview.ends {
        None => "ends an unknown number of sessions".to_string(),
        Some(ends) => format!("ends {}", plural(ends, "running session")),
    };
    if let Some(agents) = overview.running_agents.filter(|agents| *agents > 0) {
        line.push_str(&format!(", {} among them", plural(agents, "running agent")));
    }
    if let Some(kept) = overview.exited_kept.filter(|kept| *kept > 0) {
        line.push_str(&format!(" (and {kept} exited, kept)"));
    }
    match overview.other_instances {
        Some(0) => {}
        Some(others) => line.push_str(&format!(
            "; {} connected, whose tabs end too",
            if others == 1 {
                "1 other CodeSense instance is".to_string()
            } else {
                format!("{others} other CodeSense instances are")
            }
        )),
        None if overview.state == TerminalServerRun::Running => {
            line.push_str("; can't tell whether other CodeSense instances are connected")
        }
        None => {}
    }
    line
}

/// The overview as a tool result: its fields plus `ends`, in words; its
/// `expected` names the other instances connected too, which a stop or
/// restart binds to as well.
fn with_ends(overview: &TerminalServerOverview) -> serde_json::Value {
    let mut value = serde_json::json!(overview);
    value["ends"] = serde_json::json!(ends_line(overview));
    if value["expected"].is_object() {
        value["expected"]["otherInstances"] = serde_json::json!(overview.other_instances);
    }
    value
}

/// Refused, with what holds now, when the other instances connected changed
/// since `reviewed` was read: their tabs end too, so the confirmation no
/// longer says what ends.
fn check_others(reviewed: Option<u32>, now: &TerminalServerOverview) -> Result<(), String> {
    if reviewed == now.other_instances {
        return Ok(());
    }
    let now_says = match now.other_instances {
        Some(others) => others.to_string(),
        None => "unknown".to_string(),
    };
    Err(format!(
        "the other CodeSense instances connected changed since you read the status (now {now_says}; it {}): nothing ended — read terminal_server_status again",
        ends_line(now)
    ))
}

/// `terminal_server_status`.
pub(crate) fn status<R: Runtime>(app: &AppHandle<R>) -> serde_json::Value {
    with_ends(&pty_backend::terminal_server_overview(app))
}

/// `stop_terminal_server`: refused without the confirmation or `expected`.
/// The result carries what was confirmed to end (`ends`, `otherInstances`,
/// as read right before) and `ended`.
pub(crate) fn stop<R: Runtime>(
    app: &AppHandle<R>,
    params: EndTerminalServerParams,
    sink: Arc<dyn TerminalEventSink>,
    terminate: impl FnOnce(u32) -> Result<(), String>,
) -> Result<serde_json::Value, String> {
    if !params.confirm_end_sessions {
        return Err("confirmEndSessions must be true".into());
    }
    let expected = params
        .expected
        .ok_or("expected is required: pass it as terminal_server_status returned it")?;
    let reviewed_others = expected.other_instances;
    let expected: LocalActivityUpgrade = expected.into();
    let before = pty_backend::terminal_server_overview(app);
    check_others(reviewed_others, &before)?;
    let stopped = pty_backend::stop_terminal_server_with(app, &expected, sink, terminate)
        .map_err(|error| error.message)?;
    Ok(serde_json::json!({
        "ended": stopped.ended,
        "ends": ends_line(&before),
        "otherInstances": reviewed_others,
    }))
}

/// `restart_terminal_server`: refused without the confirmation, and for a
/// running server without `expected`. An incompatible one restarts the way
/// the restart prompt does (`restart_incompatible`). The result says how many
/// sessions ended (`ended`, `null` when the old server did not say) and what
/// was confirmed to end.
pub(crate) fn restart<R: Runtime>(
    app: &AppHandle<R>,
    params: EndTerminalServerParams,
    sink: Arc<dyn TerminalEventSink>,
    terminate: impl FnOnce(u32) -> Result<(), String>,
    launch: impl Fn(&Path) -> Result<(), String>,
    restart_incompatible: impl FnOnce() -> Result<Option<u32>, RestartError>,
) -> Result<serde_json::Value, String> {
    if !params.confirm_end_sessions {
        return Err("confirmEndSessions must be true".into());
    }
    let before = pty_backend::terminal_server_overview(app);
    if let Some(reviewed) = &params.reviewed_incompatible {
        ReviewedIncompatible::check(&reviewed, &before)?;
    }
    let ended = match params.expected {
        Some(expected) => {
            check_others(expected.other_instances, &before)?;
            let expected: LocalActivityUpgrade = expected.into();
            let ended = expected.session_ids.len() as u32;
            pty_backend::replace_terminal_server_with(app, &expected, sink, terminate, launch)
                .map_err(|error| error.message)?;
            Some(ended)
        }
        None if before.state == TerminalServerRun::Incompatible => {
            restart_incompatible().map_err(|error| error.message)?
        }
        None => {
            return Err(
                "expected is required to restart a running terminal server: pass it as terminal_server_status returned it"
                    .into(),
            )
        }
    };
    Ok(serde_json::json!({
        "restarted": true,
        "ended": ended,
        "ends": ends_line(&before),
        "otherInstances": before.other_instances,
    }))
}

/// `end_unowned_terminal_session`: refused without the confirmation; the
/// terminal server checks, as it ends it, that no app holds it.
pub(crate) fn end_unowned<R: Runtime>(
    app: &AppHandle<R>,
    params: EndUnownedSessionParams,
) -> Result<serde_json::Value, String> {
    if !params.confirm_end_session {
        return Err("confirmEndSession must be true".into());
    }
    pty_backend::end_unowned_terminal_session(app, &params.session)?;
    Ok(serde_json::json!({ "ended": params.session }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_restart_binds_to_the_reviewed_server_and_counts() {
        let mut now = TerminalServerOverview::bare(TerminalServerRun::Incompatible, None);
        now.pid = Some(42);
        now.ends = Some(2);
        now.other_instances = Some(1);
        let reviewed = ReviewedIncompatible::from_overview(&now);
        assert!(reviewed.check(&now).is_ok(), "unchanged restart is allowed");
        now.pid = Some(43);
        let error = reviewed.check(&now).unwrap_err();
        assert!(error.contains("changed since you read the status"));
        assert!(
            error.contains("\"pid\":43"),
            "refusal carries current state"
        );
        now.pid = reviewed.pid;
        now.ends = Some(3);
        assert!(reviewed.check(&now).is_err(), "new sessions refuse restart");
        now.ends = None;
        assert!(reviewed.check(&now).is_err(), "unknown is not zero");
        now.ends = reviewed.sessions;
        now.other_instances = Some(2);
        assert!(reviewed.check(&now).is_err());
        now.other_instances = reviewed.other_instances;
        now.state = TerminalServerRun::Running;
        assert!(reviewed.check(&now).is_err());
        now.state = TerminalServerRun::Incompatible;
        now.ends = None;
        let unknown = ReviewedIncompatible::from_overview(&now);
        assert!(unknown.check(&now).is_ok());
        now.ends = Some(0);
        assert!(unknown.check(&now).is_err());
    }

    /// What ending the server ends, in words: the running sessions, the
    /// running agents among them, the exited ones kept, and the other
    /// instances whose tabs end too; an unknown count or an unknown set of
    /// other instances is said to be unknown.
    #[test]
    fn sv7_the_confirmation_names_what_ends() {
        let mut overview = TerminalServerOverview::bare(TerminalServerRun::Running, None);
        overview.ends = Some(3);
        overview.running_agents = Some(2);
        overview.exited_kept = Some(1);
        overview.other_instances = Some(1);
        assert_eq!(
            ends_line(&overview),
            "ends 3 running sessions, 2 running agents among them (and 1 exited, kept); 1 other CodeSense instance is connected, whose tabs end too"
        );
        let mut older = TerminalServerOverview::bare(TerminalServerRun::Running, None);
        older.ends = Some(1);
        assert_eq!(
            ends_line(&older),
            "ends 1 running session; can't tell whether other CodeSense instances are connected"
        );
        let unknown = TerminalServerOverview::bare(TerminalServerRun::Unreachable, None);
        assert_eq!(ends_line(&unknown), "ends an unknown number of sessions");
    }

    /// A stop or restart takes `expected` exactly as the status returned it,
    /// and nothing else.
    #[test]
    fn sv7_a_stop_binds_to_the_status_expected() {
        let params: EndTerminalServerParams = serde_json::from_value(serde_json::json!({
            "confirmEndSessions": true,
            "expected": {"serverId": "s", "epoch": "e", "pid": 42, "sessionIds": ["a"], "otherInstances": 0},
        }))
        .unwrap();
        assert!(params.confirm_end_sessions);
        let expected: LocalActivityUpgrade = params.expected.unwrap().into();
        assert_eq!(
            expected,
            LocalActivityUpgrade {
                server_id: "s".into(),
                epoch: "e".into(),
                pid: 42,
                session_ids: vec!["a".into()],
            }
        );
        assert!(
            serde_json::from_value::<EndTerminalServerParams>(serde_json::json!({
                "confirmEndSessions": true,
                "force": true,
            }))
            .is_err()
        );
    }
}
