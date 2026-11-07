// SPDX-License-Identifier: MIT
//! SW-DX-17a — durable agent-lifecycle audit trail.
//!
//! Every agent session CodeSense spawns or kills appends ONE JSONL line to
//! `{codesense_home}/agent-audit.jsonl`, so the lifecycle of a background /
//! headless agent (one with no visible terminal tab) is reconstructable from the
//! file alone. This is the audit half of the "observe + kill" surface: the kill
//! command routes through the one `pty::kill_session` choke, and every registry
//! entry / exit passes one of the runtime entry points that call [`append`].
//!
//! **No existing durable audit facility.** Session/spawn/kill code used only
//! `tracing`/`log!` to stdout, and SW-DX-11's FE `logAction` logs dashboard
//! *user intent*, not backend session lifecycle (and misses app-opened
//! `register`ed terminals). This module is the net-new durable record.
//!
//! **Best-effort, never fatal, never on a hot path.** The append opens the file
//! `create(true).append(true)`, writes one line, and drops the handle. Any error
//! (unwritable home, disk full) is a `warn!` and a return — a spawn or kill is
//! NEVER failed because its audit line could not be written. Callers invoke this
//! OUTSIDE any registry lock so disk I/O never extends a critical section.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::launch_telemetry::LaunchTelemetry;
use super::paths::codesense_home_dir;

/// The lifecycle event a line records.
#[derive(Debug, Clone, Copy)]
pub enum AuditEvent {
    /// A session became addressable in the launch registry (bind or register).
    Spawn,
    /// A registered session was explicitly killed / cleared (through
    /// `kill_session` → `release_for_session`) — i.e. deliberate human action.
    Kill,
    /// RF-perf-6: a naturally-exited, unviewed session was automatically reclaimed
    /// by the dead-session cap (aged out beyond `PTY_DEAD_SESSION_CAP`). Distinct
    /// from [`Kill`](AuditEvent::Kill) so the audit trail never attributes a
    /// human kill to a background memory-reclaim the user did not perform.
    Expire,
    /// A reserved-but-unbound launch was aborted on the FE failure path (a
    /// `begin` slot freed without a session ever spawning).
    Abort,
}

impl AuditEvent {
    fn as_str(self) -> &'static str {
        match self {
            AuditEvent::Spawn => "spawn",
            AuditEvent::Kill => "kill",
            AuditEvent::Expire => "expire",
            AuditEvent::Abort => "abort",
        }
    }
}

/// `{codesense_home}/agent-audit.jsonl`, or `None` if the home dir is
/// unresolvable (headless CI with no `$HOME`) — in which case auditing no-ops.
fn audit_path() -> Option<PathBuf> {
    codesense_home_dir().map(|h| h.join("agent-audit.jsonl"))
}

/// Append one audit line for `event` to the default audit file. Best-effort:
/// resolves the path, delegates to [`append_to`], and swallows a missing home.
///
/// `handle` is the opaque registry handle; `session_id` is the bound PTY session
/// (absent for an `Abort` that never bound); `kind` is
/// `"dashboard-launched"` / `"registered"` (or `""` when not meaningful).
pub fn append(event: AuditEvent, handle: &str, session_id: Option<&str>, kind: &str) {
    let Some(path) = audit_path() else {
        return;
    };
    append_to(&path, event, handle, session_id, kind, None);
}

/// append one audit line carrying an optional `reason` string
/// (e.g. `"maxRuntime"` for an enforcement kill). The reason is written
/// alongside the other fields, so a reader scanning the audit file for the
/// cause of a kill never has to join a Kill line to a separate record.
pub fn append_with_reason(
    event: AuditEvent,
    handle: &str,
    session_id: Option<&str>,
    kind: &str,
    reason: Option<&str>,
) {
    let Some(path) = audit_path() else {
        return;
    };
    append_to_with_reason(&path, event, handle, session_id, kind, None, reason);
}

/// SW-WA-F11 — append one audit line carrying a [`LaunchTelemetry`] record.
/// Same best-effort semantics as [`append`]; the telemetry object rides the
/// line under a `telemetry` key so a retrospect reader can pick it off without
/// a schema change on the rest of the line.
pub fn append_with_telemetry(
    event: AuditEvent,
    handle: &str,
    session_id: Option<&str>,
    kind: &str,
    telemetry: &LaunchTelemetry,
) {
    let Some(path) = audit_path() else {
        return;
    };
    append_to(&path, event, handle, session_id, kind, Some(telemetry));
}

/// The path-injected core (unit-testable against a tempdir). Non-fatal on every
/// failure: creates the parent dir, opens append-only, writes one JSON line +
/// newline. A write error is logged and dropped.
pub fn append_to(
    path: &Path,
    event: AuditEvent,
    handle: &str,
    session_id: Option<&str>,
    kind: &str,
    telemetry: Option<&LaunchTelemetry>,
) {
    append_to_with_reason(path, event, handle, session_id, kind, telemetry, None);
}

/// like [`append_to`] but with an optional `reason` string written
/// alongside the other fields.
pub fn append_to_with_reason(
    path: &Path,
    event: AuditEvent,
    handle: &str,
    session_id: Option<&str>,
    kind: &str,
    telemetry: Option<&LaunchTelemetry>,
    reason: Option<&str>,
) {
    if let Some(parent) = path.parent() {
        // Best-effort: if the dir can't be created the open below just fails and
        // we warn — never propagate.
        let _ = std::fs::create_dir_all(parent);
    }
    let mut line = serde_json::json!({
        "event": event.as_str(),
        "handle": handle,
        "sessionId": session_id,
        "kind": kind,
        // OS user as a cheap, ADVISORY "who" (read from the env, not the OS uid —
        // a reader must not over-trust it). `USER` on unix, `USERNAME` on Windows;
        // absent if neither is set.
        "who": std::env::var("USER").or_else(|_| std::env::var("USERNAME")).ok(),
        "ts": now_millis(),
    });
    if let Some(reason) = reason {
        if let Some(obj) = line.as_object_mut() {
            obj.insert(
                "reason".into(),
                serde_json::Value::String(reason.to_string()),
            );
        }
    }
    if let Some(telemetry) = telemetry {
        if let Some(obj) = line.as_object_mut() {
            // Best-effort: if serialization fails for any reason the line is still
            // written without the telemetry key (never fail an audit append on it)
            // — but a `warn!` so a reader can see it when it happens (silently
            // swallowing it would make a telemetry-shape regression invisible).
            match serde_json::to_value(telemetry) {
                Ok(value) => {
                    obj.insert("telemetry".into(), value);
                }
                Err(e) => {
                    log::warn!(
                        target: "agent_audit",
                        "launch telemetry serialize failed: {e} — writing line without telemetry key"
                    );
                }
            }
        }
    }
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{line}") {
                log::warn!(target: "agent_audit", "audit append write failed: {e}");
            }
        }
        Err(e) => {
            log::warn!(target: "agent_audit", "audit append open failed for {}: {e}", path.display());
        }
    }
}

/// Unix epoch milliseconds; `0` if the clock is before the epoch (never in
/// practice). Kept infallible so a clock oddity can't fail an audit line.
fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_lines(path: &Path) -> Vec<serde_json::Value> {
        let body = std::fs::read_to_string(path).expect("audit file should exist");
        body.lines()
            .map(|l| serde_json::from_str(l).expect("each line is valid json"))
            .collect()
    }

    #[test]
    fn append_writes_one_jsonl_line_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-audit.jsonl");
        append_to(
            &path,
            AuditEvent::Spawn,
            "SW-A",
            Some("sess-1"),
            "dashboard-launched",
            None,
        );
        append_to(
            &path,
            AuditEvent::Kill,
            "SW-A",
            Some("sess-1"),
            "dashboard-launched",
            None,
        );

        let lines = read_lines(&path);
        assert_eq!(lines.len(), 2, "one line per append");
        assert_eq!(lines[0]["event"], "spawn");
        assert_eq!(lines[0]["handle"], "SW-A");
        assert_eq!(lines[0]["sessionId"], "sess-1");
        assert_eq!(lines[0]["kind"], "dashboard-launched");
        assert!(lines[0]["ts"].is_number(), "ts present");
        assert!(
            lines[0].get("telemetry").is_none(),
            "no telemetry key without a record"
        );
        assert_eq!(lines[1]["event"], "kill");
    }

    #[test]
    fn append_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // A nested, not-yet-created home layout — append must mkdir -p.
        let path = dir
            .path()
            .join("deep")
            .join("nested")
            .join("agent-audit.jsonl");
        append_to(
            &path,
            AuditEvent::Spawn,
            "epic/README.md",
            Some("sess-epic"),
            "registered",
            None,
        );
        let lines = read_lines(&path);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["kind"], "registered");
    }

    #[test]
    fn abort_line_allows_absent_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-audit.jsonl");
        append_to(
            &path,
            AuditEvent::Abort,
            "SW-A",
            None,
            "dashboard-launched",
            None,
        );
        let lines = read_lines(&path);
        assert_eq!(lines[0]["event"], "abort");
        assert!(lines[0]["sessionId"].is_null(), "no session id → null");
    }

    /// SW-WA-F11 — a spawn line can carry a `LaunchTelemetry` object alongside
    /// its existing fields; a reader that doesn't know about telemetry still
    /// reads the line, and a reader that does picks it off whole.
    #[test]
    fn append_with_telemetry_writes_the_record_as_a_sibling_key() {
        use super::super::launch_telemetry::{LaunchMode, LaunchOutcome, LaunchTelemetryBuilder};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-audit.jsonl");

        let mut builder = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        );
        // Shell-mode launch: the prompt renders (first byte) BEFORE the
        // settle→write protocol injects the command. The echo guard in
        // `observe_first_byte` drops post-inject bytes, so place the first
        // byte at 30ms and the injection at 55ms.
        builder.observe_first_byte(builder.started_at() + Duration::from_millis(30));
        builder.observe_shell_injected(builder.started_at() + Duration::from_millis(55));
        builder.observe_agent_live(builder.started_at() + Duration::from_millis(410));
        let telemetry = builder.finish(LaunchOutcome::Succeeded, None);

        append_to(
            &path,
            AuditEvent::Spawn,
            "SW-A",
            Some("sess-1"),
            "dashboard-launched",
            Some(&telemetry),
        );

        let lines = read_lines(&path);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line["event"], "spawn");
        assert_eq!(line["sessionId"], "sess-1");
        let t = &line["telemetry"];
        assert_eq!(t["launchMode"], "shell");
        assert_eq!(t["metricsVersion"], 3);
        assert_eq!(t["outcome"], "succeeded");
        assert_eq!(t["settleMs"], 55);
        assert_eq!(t["ptyFirstByteMs"], 30);
        assert_eq!(t["agentLiveMs"], 410);
        assert_eq!(t["sessionId"], "sess-1");
    }

    /// a Kill audit line carries its `reason` sibling so a
    /// reader scanning the file for the cause of a kill never has to join a
    /// separate record. Non-Kill events don't gain a bogus `reason` key.
    #[test]
    fn append_with_reason_writes_the_reason_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-audit.jsonl");
        append_to_with_reason(
            &path,
            AuditEvent::Kill,
            "",
            Some("sess-1"),
            "dashboard-launched",
            None,
            Some("maxRuntime"),
        );
        append_to_with_reason(
            &path,
            AuditEvent::Spawn,
            "",
            Some("sess-1"),
            "dashboard-launched",
            None,
            None,
        );
        let lines = read_lines(&path);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "kill");
        assert_eq!(lines[0]["reason"], "maxRuntime");
        assert_eq!(lines[1]["event"], "spawn");
        assert!(
            lines[1].get("reason").is_none(),
            "no reason key when none is passed"
        );
    }

    #[test]
    fn append_is_non_fatal_on_an_unwritable_path() {
        // Path whose parent is a FILE, not a dir → create_dir_all + open both
        // fail. The call must return without panicking (best-effort contract).
        let dir = tempfile::tempdir().unwrap();
        let file_as_parent = dir.path().join("not-a-dir");
        std::fs::write(&file_as_parent, b"x").unwrap();
        let path = file_as_parent.join("agent-audit.jsonl");
        append_to(&path, AuditEvent::Spawn, "SW-A", Some("s"), "", None);
        assert!(!path.exists(), "nothing written, but no panic");
    }
}
