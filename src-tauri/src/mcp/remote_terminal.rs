// SPDX-License-Identifier: MIT
//! Exact-origin MCP terminal operations. Reads never acquire a writable attachment.
use crate::commands::{
    dashboard_launch::DashboardLaunchState,
    machines::RuntimeSessionReference,
    pty_backend,
    pty_layout::{app_viewer, RemoteShellRecord},
};
use crate::terminal::session_server::{
    InputDeliveryError, SessionServerClient, WritableAttachmentFailure,
};
use codesense_pty_core::domain::{ClientBootId, TerminalDomain};
use serde_json::{json, Value};
use tauri::Manager;

pub(crate) enum RemoteOperation {
    /// Typed input: a shell command, or a prompt when an agent TUI has the tab.
    Run {
        command: String,
        target: InputTarget,
    },
    /// Paste into the input without pressing Enter.
    Type { text: String },
    /// One key, alone (e.g. the Enter that submits what is already typed).
    Key { key: TerminalKey },
    /// `screen`: only the rows on screen now, not the scrollback.
    Read { since: Option<u64>, screen: bool },
}

pub(crate) fn cursor(since: Option<&str>) -> Result<Option<u64>, &'static str> {
    since
        .map(|value| {
            let parsed = value
                .parse::<u64>()
                .map_err(|_| "since must be a decimal u64 string")?;
            if parsed.to_string() != value {
                return Err("since must be a canonical decimal u64 string");
            }
            Ok(parsed)
        })
        .transpose()
}

/// Who the caller says reads the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputTarget {
    /// Whatever runs there: the agent protocol when the tab was launched as an
    /// agent or an agent TUI is in its foreground, otherwise the shell protocol.
    Auto,
    /// An agent session (`send_to_session`): always the agent protocol.
    Agent,
}

/// A key `run_in_terminal_tab` can send on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalKey {
    Enter,
    Escape,
    Tab,
    ShiftTab,
    Backspace,
    CtrlC,
    CtrlD,
    Up,
    Down,
    Left,
    Right,
}

impl TerminalKey {
    /// The bytes a terminal sends for the key.
    pub(crate) fn bytes(self) -> &'static [u8] {
        match self {
            Self::Enter => b"\r",
            Self::Escape => b"\x1b",
            Self::Tab => b"\t",
            Self::ShiftTab => b"\x1b[Z",
            Self::Backspace => b"\x7f",
            Self::CtrlC => b"\x03",
            Self::CtrlD => b"\x04",
            Self::Up => b"\x1b[A",
            Self::Down => b"\x1b[B",
            Self::Right => b"\x1b[C",
            Self::Left => b"\x1b[D",
        }
    }
}

fn refused(code: &str, message: impl ToString) -> Value {
    json!({"status":"refused","code":code,"message":message.to_string()})
}

/// The adapter enters here only after resolving an opaque handle. None is local-only.
pub(crate) async fn by_handle(
    app: tauri::AppHandle,
    handle: &str,
    operation: RemoteOperation,
) -> Option<Value> {
    by_handle_bound(app, handle, operation, None).await
}

pub(crate) async fn by_handle_bound(
    app: tauri::AppHandle,
    handle: &str,
    operation: RemoteOperation,
    caller: Option<super::remote_authority::RemoteAuthority>,
) -> Option<Value> {
    let input = operation.is_input();
    let record = app
        .try_state::<DashboardLaunchState>()?
        .remote_shell_by_handle(handle)?;
    if input {
        if let Some(caller) = &caller {
            if caller.validate().await.is_err() || caller.check_control(&record).is_err() {
                return Some(input_result(
                    refused(
                        "accessDenied",
                        "target MCP grant is outside caller scope or unavailable",
                    ),
                    true,
                ));
            }
        }
    }
    let reference = RuntimeSessionReference::Admitted(Box::new(record.target.clone()));
    if let Err(error) =
        crate::commands::pty_layout::authorize_human_terminal(&app, &reference).await
    {
        return Some(input_result(refused("accessDenied", error), input));
    }
    let input_app = app.clone();
    let input_reference = reference.clone();
    let operation = crate::commands::await_blocking("mcp_remote_terminal", move || {
        let backend = pty_backend::resolve_session_reference(&app, &reference, true)?;
        backend.revalidate(&app)?;
        let client = backend
            .client()
            .filter(|_| backend.is_remote())
            .ok_or("remote admission unavailable")?;
        let lease = client
            .private_lease()
            .ok_or("private authority unavailable")?;
        lease.validate()?;
        if lease.owner_id() != record.owner_id {
            return Err("original private owner unavailable".into());
        }
        Ok(execute(&client, &record, operation, || {
            if input {
                if let Some(caller) = &caller {
                    caller.check_control(&record)?;
                }
            }
            backend.revalidate(&app)?;
            crate::commands::pty_layout::validate_remote_shell_group(&app, &record)
        }))
    });
    let result = if input {
        crate::commands::pty::submit_automatic_input(input_app, input_reference, operation).await
    } else {
        operation.await
    };
    Some(input_result(
        result.unwrap_or_else(|error| refused("staleOrigin", error)),
        input,
    ))
}

/// Same production dispatch is used by the owned SSH fixture. The revalidation
/// closure binds all operations to the captured admitted connection, never selection.
pub(crate) fn execute(
    client: &SessionServerClient,
    record: &RemoteShellRecord,
    operation: RemoteOperation,
    revalidate: impl Fn() -> Result<(), String>,
) -> Value {
    let input = operation.is_input();
    input_result(execute_inner(client, record, operation, revalidate), input)
}

impl RemoteOperation {
    fn is_input(&self) -> bool {
        matches!(
            self,
            Self::Run { .. } | Self::Type { .. } | Self::Key { .. }
        )
    }

    fn validate(&self) -> Result<(), Value> {
        let text = match self {
            Self::Run { command, .. } => command,
            Self::Type { text } => text,
            _ => return Ok(()),
        };
        if codesense_pty_core::domain::is_blank_agent_input(text) {
            return Err(refused(
                "invalid",
                "command must not be empty; send a lone Enter with key \"enter\"",
            ));
        }
        Ok(())
    }
}

/// Claim the tab's input on this connection and re-check the origin before writing.
fn claim_input(
    client: &SessionServerClient,
    record: &RemoteShellRecord,
    revalidate: &impl Fn() -> Result<(), String>,
) -> Result<(), Value> {
    if let Err(error) = client.claim_writable_typed(&record.target.session) {
        return Err(refused(
            match error {
                WritableAttachmentFailure::AttachedElsewhere => "attachedElsewhere",
                _ => "unavailable",
            },
            error,
        ));
    }
    revalidate().map_err(|error| refused("staleOrigin", error))
}

/// The delivery result of typed input, never claiming more certainty than known.
fn delivery(
    delivered: Result<bool, InputDeliveryError>,
    route: Option<crate::commands::pty::InputRoute>,
    submit: bool,
    revalidate: impl FnOnce() -> Result<(), String>,
) -> Value {
    if revalidate().is_err() {
        return json!({"status":"unknown","submitted":null,"code":"staleOrigin"});
    }
    let mut value = match delivered {
        Ok(true) => json!({"status":if submit { "submitted" } else { "typed" },"submitted":submit}),
        Ok(false) => json!({"status":"notSent","submitted":false,"code":"notReady"}),
        Err(InputDeliveryError::NotSent(message)) => {
            json!({"status":"notSent","submitted":false,"code":"unavailable","message":message})
        }
        Err(InputDeliveryError::Unknown(message)) => {
            json!({"status":"unknown","submitted":null,"code":"deliveryUnknown","message":message})
        }
    };
    if let Some(route) = route {
        value["input"] = json!(route.as_str());
    }
    value
}

fn input_result(mut value: Value, input: bool) -> Value {
    if input && value.get("submitted").is_none() {
        value["status"] = json!("refused");
        value["submitted"] = json!(false);
    }
    value
}

fn execute_inner(
    client: &SessionServerClient,
    record: &RemoteShellRecord,
    operation: RemoteOperation,
    revalidate: impl Fn() -> Result<(), String>,
) -> Value {
    if let Err(refusal) = operation.validate() {
        return refusal;
    }
    if let Err(error) = revalidate() {
        return refused("staleOrigin", error);
    }
    let snapshot = match client.layout_snapshot(&app_viewer(), &ClientBootId::new("mcp-remote")) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return json!({"status":"unavailable","code":"unavailable","message":error.to_string()})
        }
    };
    let Some(tab) = snapshot
        .workspaces
        .get(&record.workspace)
        .and_then(|workspace| {
            workspace
                .tabs
                .iter()
                .find(|tab| tab.session == record.target.session)
        })
    else {
        return json!({"status":"unavailable","code":"correlationUnavailable","message":"Original launch correlation is not currently visible; attach using the original request or handle."});
    };
    if tab.tags.get("remoteHandle") != Some(&record.handle)
        || tab.tags.get("remoteRequestId") != Some(&record.request_id.to_string())
    {
        return refused("staleOrigin", "original tab correlation changed");
    }
    if tab.state.is_exited() {
        return json!({"status":"ended","code":"ended"});
    }
    match operation {
        RemoteOperation::Run { command, target } => {
            if let Err(refusal) = claim_input(client, record, &revalidate) {
                return refusal;
            }
            let route = crate::commands::pty::InputRoute::for_tab(
                target == InputTarget::Agent || !record.launch.is_shell(),
                || crate::commands::pty::agent_in_foreground(client, &record.target.session),
            );
            let submitted = submit_remote_line(client, &record.target.session, route, &command);
            delivery(
                submitted.map(|outcome| outcome.submitted),
                Some(route),
                true,
                &revalidate,
            )
        }
        RemoteOperation::Type { text } => {
            if let Err(refusal) = claim_input(client, record, &revalidate) {
                return refusal;
            }
            let written = client.write_input_typed(
                &record.target.session,
                crate::commands::pty::bracketed_paste(&text).as_bytes(),
            );
            delivery(written.map(|()| true), None, false, &revalidate)
        }
        RemoteOperation::Key { key } => {
            if let Err(refusal) = claim_input(client, record, &revalidate) {
                return refusal;
            }
            let written = client.write_input_typed(&record.target.session, key.bytes());
            delivery(written.map(|()| true), None, true, &revalidate)
        }
        RemoteOperation::Read { since, screen } => {
            if let Err(error) = revalidate() {
                return refused("staleOrigin", error);
            }
            // What the tab shows, from the host's emulator terminal. There is no
            // byte history to resume from, so every read is a whole read
            // (`reset: true`); `since` only guards against a cursor past `head`.
            let text = if screen {
                codesense_pty_core::domain::screen_text(client, &record.target.session)
            } else {
                codesense_pty_core::domain::visible_text(client, &record.target.session, u32::MAX)
            };
            if let Err(error) = revalidate() {
                return refused("staleOrigin", error);
            }
            match text {
                Ok(text) if since.is_some_and(|since| since > text.seq) => {
                    refused("invalidCursor", "since is beyond the output head")
                }
                Ok(text) => json!({
                    "status": "ok",
                    "output": text.text,
                    "head": text.seq.to_string(),
                    "reset": true,
                    "altScreen": text.alt_screen,
                }),
                Err(error) => {
                    json!({"status":"unavailable","code":"unavailable","message":error.to_string()})
                }
            }
        }
    }
}

/// Route from the tab's launch choice and what runs in it now, never from current
/// UI selection.
pub(crate) fn submit_remote_line(
    client: &SessionServerClient,
    session: &codesense_pty_core::domain::PtySessionId,
    route: crate::commands::pty::InputRoute,
    line: &str,
) -> Result<codesense_pty_core::domain::SubmitOutcome, InputDeliveryError> {
    use crate::commands::mcp_workspace::{
        SUBMIT_CONFIRM_MS, SUBMIT_MAX_NUDGES, SUBMIT_QUIET_MS, SUBMIT_TIMEOUT_MS,
    };
    use crate::commands::pty::InputRoute;
    match route {
        InputRoute::Shell => {
            client.submit_shell_line_typed(session, line, SUBMIT_QUIET_MS, SUBMIT_TIMEOUT_MS)
        }
        InputRoute::Agent => client.submit_agent_line_typed(
            session,
            line,
            SUBMIT_QUIET_MS,
            SUBMIT_CONFIRM_MS,
            SUBMIT_TIMEOUT_MS,
            SUBMIT_MAX_NUDGES,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_claims_input_and_does_not_submit_until_enter() {
        use crate::terminal::session_server::SessionServerState;
        use codesense_pty_core::domain::{PtySessionId, ViewerId};
        use codesense_pty_core::layout::{OpenTab, Placement, WorkspaceDescriptor};
        use codesense_session_server::testing::{
            registration, sh, wait_until, RecordingSink, TestServer,
        };
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let server = TestServer::start();
        let client = SessionServerState::default()
            .connect_or_launch(server.root(), Arc::new(RecordingSink::default()), |_| {
                Err("no launch expected".into())
            })
            .unwrap();
        let session = PtySessionId::new("typed-prompt");
        server.track(session.as_str());
        let record: RemoteShellRecord = serde_json::from_value(json!({
            "handle":"remote-tab", "owner_id":41, "request_id":uuid::Uuid::new_v4(),
            "machine_id":"remote", "team_key":"org:team", "workspace_id":"work",
            "workspace":"org:team|work", "cwd":"/tmp", "name":null, "launch":"claude",
            "target":{"member":{"machineId":"remote","slot":"remote"},"admissionRevision":"1",
                "namespace":{"serverId":"s","epoch":"e","generation":"1"},"session":session.as_str()}
        })).unwrap();
        client.open_remote_tab(OpenTab {
            viewer: ViewerId::new("app"), workspace: record.workspace.clone(), session: session.clone(),
            descriptor: Some(WorkspaceDescriptor { display_name: "work".into(), kind: "root".into(), ..Default::default() }),
            name: None, role: None,
            tags: [("remoteHandle".into(), record.handle.clone()), ("remoteRequestId".into(), record.request_id.to_string())].into(),
            placement: Placement::Tab, index: None, activate: false,
        }, || client.spawn(sh("printf 'READY\\n'; read line; printf 'RECEIVED:%s\\n' \"$line\"; cat"), registration(session.as_str(), None, false)).map(|_| ())).unwrap();
        let text = || {
            codesense_pty_core::domain::visible_text(&*client, &session, u32::MAX)
                .unwrap()
                .text
        };
        assert!(wait_until(|| text().contains("READY")));
        let typed = execute(
            &client,
            &record,
            RemoteOperation::Type {
                text: "draft".into(),
            },
            || Ok(()),
        );
        assert_eq!(typed, json!({"status":"typed", "submitted":false}));
        assert!(wait_until(|| text().contains("draft")));
        assert!(!text().contains("RECEIVED:draft"));
        let blank = execute(
            &client,
            &record,
            RemoteOperation::Type { text: " \n".into() },
            || Ok(()),
        );
        assert_eq!(blank["code"], "invalid");
        assert_eq!(blank["submitted"], false);

        let validations = AtomicUsize::new(0);
        let stale = execute(
            &client,
            &record,
            RemoteOperation::Type {
                text: " tail".into(),
            },
            || {
                if validations.fetch_add(1, Ordering::SeqCst) >= 2 {
                    Err("changed".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(stale["status"], "unknown");
        assert_eq!(stale["code"], "staleOrigin");
        assert!(stale["submitted"].is_null());
        assert!(wait_until(|| text().contains("draft tail")));
        execute(
            &client,
            &record,
            RemoteOperation::Key {
                key: TerminalKey::Enter,
            },
            || Ok(()),
        );
        assert!(wait_until(|| text().contains("RECEIVED:draft tail")));
        client.sever();
    }

    #[test]
    fn remote_mcp_cursors_are_lossless_and_strict() {
        assert_eq!(
            cursor(Some("18446744073709551615")).unwrap(),
            Some(u64::MAX)
        );
        assert_eq!(
            cursor(Some("9007199254740993")).unwrap(),
            Some(9007199254740993)
        );
        assert_eq!(cursor(None).unwrap(), None);
        for invalid in ["", "01", "-1", "1.0", "18446744073709551616"] {
            assert!(cursor(Some(invalid)).is_err());
        }
    }
    #[test]
    fn each_key_sends_the_bytes_a_terminal_sends_for_it() {
        for (key, bytes) in [
            (TerminalKey::Enter, &b"\r"[..]),
            (TerminalKey::Escape, b"\x1b"),
            (TerminalKey::Tab, b"\t"),
            (TerminalKey::ShiftTab, b"\x1b[Z"),
            (TerminalKey::Backspace, b"\x7f"),
            (TerminalKey::CtrlC, b"\x03"),
            (TerminalKey::CtrlD, b"\x04"),
            (TerminalKey::Up, b"\x1b[A"),
            (TerminalKey::Down, b"\x1b[B"),
            (TerminalKey::Right, b"\x1b[C"),
            (TerminalKey::Left, b"\x1b[D"),
        ] {
            assert_eq!(key.bytes(), bytes, "{key:?}");
        }
        let names: Vec<TerminalKey> = serde_json::from_value(json!([
            "enter",
            "escape",
            "tab",
            "shift-tab",
            "backspace",
            "ctrl-c",
            "ctrl-d",
            "up",
            "down",
            "left",
            "right"
        ]))
        .unwrap();
        assert_eq!(names.len(), 11);
    }

    #[test]
    fn key_delivery_reports_certainty_and_no_route() {
        assert_eq!(
            delivery(Ok(true), None, true, || Ok(())),
            json!({"status":"submitted","submitted":true})
        );
        let unknown = delivery(
            Err(InputDeliveryError::Unknown("lost".into())),
            None,
            true,
            || Ok(()),
        );
        assert_eq!(unknown["status"], "unknown");
        assert!(unknown["submitted"].is_null());
        let not_sent = delivery(
            Err(InputDeliveryError::NotSent("closed".into())),
            None,
            true,
            || Ok(()),
        );
        assert_eq!(not_sent["submitted"], false);
        assert_eq!(
            delivery(
                Ok(true),
                Some(crate::commands::pty::InputRoute::Agent),
                true,
                || Ok(())
            )["input"],
            "agent"
        );
    }

    #[test]
    fn remote_mcp_input_failure_never_fabricates_submission() {
        assert_eq!(
            input_result(refused("staleOrigin", "changed"), true)["submitted"],
            false
        );
        assert_eq!(
            input_result(json!({"status":"unknown","submitted":null}), true)["status"],
            "unknown"
        );
        assert!(input_result(refused("staleOrigin", "changed"), false)
            .get("output")
            .is_none());
    }
}
