// SPDX-License-Identifier: MIT
//! The machine's own answer to a session's CodeSense MCP while no desktop
//! relays it: a remote agent's CodeSense MCP is reverse-proxied
//! through the desktop, so with the Mac closed (or for an agent this machine
//! started while no desktop was connected) the machine answers two tools
//! itself, for the session the bridge authenticated:
//!
//! - `routine_done {runId, outcome, summary}`: finishes that routine run, if
//!   this machine fired it into (or started) this session;
//! - `signal_done {status, summary}`: finishes the routine run this machine
//!   fired into this session most recently and not yet finished.
//!
//! Both are recorded in the routine run log, so the outcome reaches the
//! desktop with the run rows when it reconnects. Every other tool answers a
//! typed `desktop_offline` error: the MCP server stays up and says why,
//! instead of disappearing. The machine also keeps the session's own timers
//! and an outbox (`crate::offline_mcp`), and hands the stream back to the desktop
//! when it returns.
//!
//! Only the CodeSense server answers those two: another MCP server a desktop
//! relays lists no tools and answers every call `desktop_offline`.

use serde_json::{json, Value};

use super::RoutineArms;

/// The error a desktop-relayed tool answers while no desktop is connected: its code
/// and `data.type`, shared with the bridge that tells it from a refusal.
pub use codesense_host_proto::reverse_mcp::{DESKTOP_OFFLINE, DESKTOP_OFFLINE_CODE};

/// Why no desktop relays a session's MCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offline {
    /// The desktop that relayed it disconnected.
    DesktopGone,
    /// This machine started the agent (a routine) while no desktop that
    /// could relay it was connected.
    StartedByMachine,
}

fn offline_message(tool: &str, why: Offline, codesense: bool) -> String {
    let answered = if codesense {
        "This machine answers routine_done, signal_done, list_wakeups and cancel_wakeup itself, and queues send_message, inbox_post and signal_progress until the desktop reconnects"
    } else {
        "This MCP server runs on your desktop"
    };
    match why {
        Offline::DesktopGone => format!(
            "desktop_offline: {tool} needs CodeSense on your desktop, which is not connected to this machine. {answered}; retry other tools once it is back."
        ),
        Offline::StartedByMachine => format!(
            "desktop_offline: {tool} needs CodeSense on your desktop, which was not connected when this machine started this session for a routine, so this session's tools are not relayed to it. {answered}; other tools work once your desktop connects (CodeSense hands this session to it)."
        ),
    }
}

/// The tools the machine answers itself.
fn tools() -> Value {
    json!([
        {
            "name": "routine_done",
            "description": "Finish one routine run this machine started or woke you for, without ending your session: outcome success | failed | needs-attention, and a summary. Recorded on the machine and synced to the desktop when it reconnects.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sessionId": {"type": "string"},
                    "runId": {"type": "string"},
                    "outcome": {"type": "string", "enum": ["success", "failed", "needs-attention"]},
                    "summary": {"type": "string"}
                },
                "required": ["runId", "outcome"]
            }
        },
        {
            "name": "signal_done",
            "description": "Report your work done: finishes the routine run this machine most recently started or woke you for. status success | failed | needs-attention, and a summary. Recorded on the machine and synced to the desktop when it reconnects.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sessionId": {"type": "string"},
                    "status": {"type": "string", "enum": ["success", "failed", "needs-attention"]},
                    "summary": {"type": "string"}
                },
                "required": ["status"]
            }
        }
    ])
}

fn result(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn error(id: &Value, code: i64, message: String, kind: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message, "data": {"type": kind}}
    })
    .to_string()
}

fn text(body: Value) -> Value {
    json!({"content": [{"type": "text", "text": body.to_string()}], "isError": false})
}

/// The machine's answer to one JSON-RPC line `line` from the agent in
/// `session`, or `None` for a notification (or a line that is not a request).
///
/// `codesense`: the stream is the CodeSense server's (another server lists
/// no tools); `arms`: this host's armed routines, when they run.
pub fn respond(
    line: &str,
    session: &str,
    arms: Option<&RoutineArms>,
    codesense: bool,
    why: Offline,
) -> Option<String> {
    let message: Value = serde_json::from_str(line.trim()).ok()?;
    let id = message.get("id")?.clone();
    if id.is_null() {
        return None;
    }
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => result(
            &id,
            json!({
                "protocolVersion": params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2024-11-05"),
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {
                    "name": if codesense { "codesense (machine, desktop may be offline)" } else { "desktop MCP server (desktop may be offline)" },
                    "version": "1"
                },
                "instructions": if codesense {
                    "CodeSense on your desktop may be offline; try the tool first. This machine answers routine_done, signal_done, list_wakeups and cancel_wakeup, and queues send_message, inbox_post and signal_progress until it reconnects. A tool that returns desktop_offline means the desktop is not connected."
                } else {
                    "This MCP server runs on your desktop, which may be offline; try the tool first. It may have no tools here."
                }
            }),
        ),
        "ping" => result(&id, json!({})),
        "tools/list" if codesense => result(&id, json!({"tools": tools()})),
        "tools/list" => result(&id, json!({"tools": []})),
        "tools/call" => {
            let tool = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            let field = |name: &str| args.get(name).and_then(Value::as_str).map(str::to_string);
            let offline = |tool: &str| {
                error(
                    &id,
                    DESKTOP_OFFLINE_CODE,
                    offline_message(tool, why, codesense),
                    DESKTOP_OFFLINE,
                )
            };
            if !codesense {
                return Some(offline(tool));
            }
            match (tool, arms) {
                ("routine_done" | "signal_done", None) => offline(tool),
                ("routine_done", Some(arms)) => {
                    let (Some(run), Some(outcome)) = (field("runId"), field("outcome")) else {
                        return Some(error(
                            &id,
                            -32602,
                            "routine_done needs runId and outcome".into(),
                            "invalid_params",
                        ));
                    };
                    match arms.complete(
                        session,
                        Some(&run),
                        &outcome,
                        &field("summary").unwrap_or_default(),
                    ) {
                        Ok(done) => result(
                            &id,
                            text(
                                json!({"status": "ok", "data": {"runId": done.run_id, "outcome": outcome, "recordedOn": "machine"}}),
                            ),
                        ),
                        Err(why) => error(&id, -32602, why, "refused"),
                    }
                }
                ("signal_done", Some(arms)) => {
                    let Some(status) = field("status") else {
                        return Some(error(
                            &id,
                            -32602,
                            "signal_done needs a status".into(),
                            "invalid_params",
                        ));
                    };
                    match arms.complete(
                        session,
                        None,
                        &status,
                        &field("summary").unwrap_or_default(),
                    ) {
                        Ok(done) => result(
                            &id,
                            text(
                                json!({"status": "ok", "data": {"runId": done.run_id, "outcome": status, "recordedOn": "machine"}}),
                            ),
                        ),
                        // Not a routine run of this machine: the desktop owns it.
                        Err(_) => offline(tool),
                    }
                }
                (tool, _) => offline(tool),
            }
        }
        other => error(
            &id,
            DESKTOP_OFFLINE_CODE,
            offline_message(
                if other.is_empty() {
                    "this request"
                } else {
                    other
                },
                why,
                codesense,
            ),
            DESKTOP_OFFLINE,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_instructions_do_not_claim_the_desktop_is_offline() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        for why in [Offline::DesktopGone, Offline::StartedByMachine] {
            for codesense in [true, false] {
                let reply = respond(line, "session", None, codesense, why).unwrap();
                let reply: Value = serde_json::from_str(&reply).unwrap();
                let text = reply["result"]["instructions"].as_str().unwrap();
                let name = reply["result"]["serverInfo"]["name"].as_str().unwrap();
                assert!(
                    text.contains("may be offline; try the tool first"),
                    "{text}"
                );
                assert!(!text.contains("not relaying"), "{text}");
                assert!(!text.contains("every other tool returns"), "{text}");
                assert!(!name.contains("desktop offline"), "{name}");
                if codesense {
                    assert!(text.contains("list_wakeups"), "{text}");
                } else {
                    assert!(text.contains("may have no tools here"), "{text}");
                }
            }
        }
    }
}
