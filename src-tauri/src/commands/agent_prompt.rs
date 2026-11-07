// SPDX-License-Identifier: MIT
//! A person sends a prompt to a running agent from the app (Notes' "Send to…").
//!
//! The picker lists the team's agents from the launch registry, local and remote
//! alike, so the send resolves its target from that same registry: an agent whose
//! tab runs on a machine is reached through the remote input path `send_to_session`
//! uses (the agent protocol, with remote delivery certainty), and an agent on this
//! Mac through the terminal's local input path. The remote result keeps
//! its certainty: `unknown` means the prompt may have been submitted, so the caller
//! must never send it again on its own.

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::commands::dashboard_launch::DashboardLaunchState;
use crate::commands::machines::RuntimeSessionReference;
use crate::commands::mcp_workspace::{
    SUBMIT_CONFIRM_MS, SUBMIT_MAX_NUDGES, SUBMIT_QUIET_MS, SUBMIT_TIMEOUT_MS,
};

/// What became of a prompt sent to an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentPromptResult {
    /// `submitted`, `typed`, `notSent`, `unknown` or `refused`. An ended remote tab
    /// or unavailable host is `refused` with its reason code. `typed` means written
    /// without a confirmed submit, including an explicit paste without Enter,
    /// locally or remotely.
    pub status: String,
    /// `true` submitted, `false` not submitted, `null` unknown.
    pub submitted: Option<bool>,
    /// The machine's label when the agent runs on one; `null` on this Mac.
    pub machine: Option<String>,
    /// The remote input path's reason code (`staleOrigin`, `notReady`,
    /// `deliveryUnknown`, `ended`, `attachedElsewhere`, …); `null` on this Mac.
    pub code: Option<String>,
    /// Why it was not sent, in words that name the machine.
    pub message: Option<String>,
}

/// Where a prompt for `handle` goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PromptRoute {
    /// A tab on a machine, by the handle its remote record is registered under.
    Remote { handle: String, machine_id: String },
    /// A session on this Mac.
    Local { session_id: String },
}

/// The route for a picked agent, in this order: its handle's remote record (a
/// machine's tab); the local session its handle is bound to (the registry is
/// authoritative over the row's possibly stale session); the remote tab running
/// the row's session (a hand-started agent in a machine's tab); and only for a
/// handle the registry does not know (a hand-started agent on this Mac), the
/// session the roster showed.
pub(crate) fn route_for(
    remote_by_handle: Option<(String, String)>,
    local_by_handle: Option<String>,
    remote_by_session: Option<(String, String)>,
    session_id: Option<&str>,
) -> Result<PromptRoute, String> {
    if let Some((handle, machine_id)) = remote_by_handle {
        return Ok(PromptRoute::Remote { handle, machine_id });
    }
    if let Some(session_id) = local_by_handle {
        return Ok(PromptRoute::Local { session_id });
    }
    if let Some((handle, machine_id)) = remote_by_session {
        return Ok(PromptRoute::Remote { handle, machine_id });
    }
    match session_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(session_id) => Ok(PromptRoute::Local {
            session_id: session_id.to_string(),
        }),
        None => Err("the agent has no running session".into()),
    }
}

/// The route for a picked agent, read from the launch registry the roster is built
/// from (see [`route_for`] for the order).
pub(crate) fn route_in(
    launch: Option<&DashboardLaunchState>,
    handle: &str,
    session_id: Option<&str>,
) -> Result<PromptRoute, String> {
    // Hand-started roster rows use their session id as the handle.
    let session_id = session_id.or(Some(handle));
    let remote =
        |record: crate::commands::pty_layout::RemoteShellRecord| (record.handle, record.machine_id);
    let by_handle = launch
        .and_then(|l| l.remote_shell_by_handle(handle))
        .map(remote);
    let local = match &by_handle {
        None => launch.and_then(|l| l.resolve(handle)),
        Some(_) => None,
    };
    let by_session = match (launch, session_id, &by_handle, &local) {
        (Some(l), Some(sid), None, None) => l
            .remote_handle_for_session(sid)
            .and_then(|h| l.remote_shell_by_handle(&h))
            .map(remote),
        _ => None,
    };
    route_for(by_handle, local, by_session, session_id)
}

/// The person's view of a remote delivery outcome (`remote_terminal`'s JSON).
pub(crate) fn remote_result(outcome: &Value, machine: &str) -> AgentPromptResult {
    let text = |key: &str| outcome.get(key).and_then(Value::as_str).map(str::to_string);
    let submitted = match outcome.get("submitted") {
        Some(Value::Bool(submitted)) => Some(*submitted),
        _ => None,
    };
    // A result with no status word keeps only what `submitted` says, never more.
    let status = text("status").unwrap_or_else(|| {
        match submitted {
            Some(true) => "submitted",
            Some(false) => "refused",
            None => "unknown",
        }
        .into()
    });
    let submitted = match (submitted, status.as_str()) {
        (Some(submitted), _) => Some(submitted),
        (None, "unknown") => None,
        (None, status) => Some(status == "submitted"),
    };
    let code = text("code");
    // Input results carry an ended tab as `refused` with code `ended`.
    let message = match (status.as_str(), code.as_deref()) {
        ("submitted" | "typed", _) => None,
        ("ended", _) | (_, Some("ended")) => Some(format!("its tab on {machine} has ended")),
        _ => Some(name_machine(
            text("message").as_deref(),
            code.as_deref(),
            machine,
        )),
    };
    AgentPromptResult {
        status,
        submitted,
        machine: Some(machine.to_string()),
        code,
        message,
    }
}

/// A connection refusal reads "machine is disconnected"; say which machine.
fn name_machine(message: Option<&str>, code: Option<&str>, machine: &str) -> String {
    let message = message
        .map(|m| m.trim().trim_end_matches('.').trim_end())
        .filter(|m| !m.is_empty());
    match message {
        Some(message) => match message
            .get(..11)
            .filter(|head| head.eq_ignore_ascii_case("machine is "))
            .map(|_| &message[11..])
        {
            Some(rest) => format!("{machine} is {rest}"),
            None if message.contains("remote runtime unavailable") => {
                format!("{machine} is not connected")
            }
            None => format!("{message} (on {machine})"),
        },
        None => match code {
            Some("notReady") => format!("the agent on {machine} was not ready for input"),
            Some(code) => format!("{machine} refused it ({code})"),
            None => format!("{machine} did not take it"),
        },
    }
}

/// The local outcome: the text was written; `submitted` says whether the agent
/// visibly took it.
pub(crate) fn local_result(submitted: bool) -> AgentPromptResult {
    AgentPromptResult {
        status: if submitted { "submitted" } else { "typed" }.into(),
        submitted: Some(submitted),
        machine: None,
        code: None,
        message: None,
    }
}

/// Send `text` to the agent the roster listed as `handle` (with `session_id`),
/// typed into its input, optionally submitted, wherever it runs.
#[tauri::command]
pub(crate) async fn send_prompt_to_agent(
    app: AppHandle,
    handle: String,
    session_id: Option<String>,
    text: String,
    submit: Option<bool>,
) -> Result<AgentPromptResult, String> {
    let submit = submit.unwrap_or(true);
    if codesense_pty_core::domain::is_blank_agent_input(&text) {
        return Err("there is nothing to send".into());
    }
    let route = route_in(
        app.try_state::<DashboardLaunchState>().as_deref(),
        &handle,
        session_id.as_deref(),
    )?;
    match route {
        PromptRoute::Remote { handle, machine_id } => {
            let machine = crate::commands::machines::machine_labels(&app)
                .await
                .remove(&machine_id)
                .unwrap_or(machine_id);
            let outcome = crate::mcp::remote_terminal::by_handle_bound(
                app.clone(),
                &handle,
                if submit {
                    crate::mcp::remote_terminal::RemoteOperation::Run {
                        command: text.clone(),
                        target: crate::mcp::remote_terminal::InputTarget::Agent,
                    }
                } else {
                    crate::mcp::remote_terminal::RemoteOperation::Type { text: text.clone() }
                },
                None,
            )
            .await
            .ok_or_else(|| format!("its tab on {machine} is no longer known"))?;
            let result = remote_result(&outcome, &machine);
            if result.submitted == Some(true) {
                log_typed(&app, &handle, &text).await;
            }
            Ok(result)
        }
        PromptRoute::Local { session_id } => {
            if !submit {
                crate::commands::pty::write_automatic_input(
                    app,
                    RuntimeSessionReference::Legacy(session_id),
                    crate::commands::pty::bracketed_paste(&text).into_bytes(),
                )
                .await?;
                return Ok(local_result(false));
            }
            let outcome = crate::commands::pty::admitted_commands::submit_pty_line(
                app.clone(),
                RuntimeSessionReference::Legacy(session_id.clone()),
                text.clone(),
                SUBMIT_QUIET_MS,
                SUBMIT_CONFIRM_MS,
                SUBMIT_TIMEOUT_MS,
                SUBMIT_MAX_NUDGES,
            )
            .await?;
            if outcome.submitted {
                // A hand-started agent has no registered handle: its session names it.
                let target = if handle.trim().is_empty() {
                    &session_id
                } else {
                    &handle
                };
                log_typed(&app, target, &text).await;
            }
            Ok(local_result(outcome.submitted))
        }
    }
}

/// Record a confirmed submit in the target's inbox conversation, from the person
/// (marked typed into the terminal), as MCP `send_to_session` does. Only a
/// confirmed submit is logged; logging never fails the send.
async fn log_typed(app: &AppHandle, target: &str, text: &str) {
    let host = crate::commands::agent_inbox::AppInboxHost { app: app.clone() };
    if let Err(e) = crate::commands::agent_inbox::log_terminal_send(&host, None, target, text).await
    {
        log::warn!(target: "inbox", "notes send to {target} not logged: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pair(a: &str, b: &str) -> Option<(String, String)> {
        Some((a.into(), b.into()))
    }

    #[test]
    fn a_remote_tab_handle_routes_to_its_machine() {
        assert_eq!(
            route_for(
                pair("remote-tab-f05", "m-test-1"),
                None,
                None,
                Some("remote-shell-9")
            ),
            Ok(PromptRoute::Remote {
                handle: "remote-tab-f05".into(),
                machine_id: "m-test-1".into()
            })
        );
    }

    #[test]
    fn a_row_known_only_by_its_remote_session_still_routes_to_its_machine() {
        assert_eq!(
            route_for(
                None,
                None,
                pair("remote-tab-7", "m-2"),
                Some("remote-shell-7")
            ),
            Ok(PromptRoute::Remote {
                handle: "remote-tab-7".into(),
                machine_id: "m-2".into()
            })
        );
    }

    #[test]
    fn a_local_agent_routes_to_its_session_on_this_mac() {
        assert_eq!(
            route_for(None, None, None, Some("sess-local")),
            Ok(PromptRoute::Local {
                session_id: "sess-local".into()
            })
        );
        assert!(route_for(None, None, None, None).is_err());
        assert!(route_for(None, None, None, Some("  ")).is_err());
    }

    #[test]
    fn a_page_can_address_a_hand_started_agent_by_its_session_handle() {
        let state = DashboardLaunchState::new(4);
        assert_eq!(
            route_in(Some(&state), "sess-hand", None),
            Ok(PromptRoute::Local {
                session_id: "sess-hand".into()
            })
        );
    }

    #[test]
    fn local_input_rejects_unknown_and_ended_session_handles() {
        use codesense_pty_core::domain::{PtySessionId, TerminalDomain};
        use codesense_pty_core::layout::ServerIdentity;
        use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};
        use codesense_pty_core::TerminalHost;
        use codesense_session_server::testing::{registration, sh_spec, wait_until, RecordingSink};
        use std::sync::Arc;

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = TerminalHost::new(
            ServerIdentity::ephemeral("test"),
            Arc::new(RecordingSink::default()),
            runtime.handle().clone(),
        );
        let pty = runtime
            .block_on(LocalPtyRunner.spawn(sh_spec("ended", "exit 0")))
            .unwrap();
        host.register(pty, registration("ended", None, false))
            .unwrap();
        assert!(wait_until(|| host
            .live_child_pids(&[PtySessionId::new("ended")])
            .unwrap()
            == vec![None]));
        for handle in ["unknown", "ended"] {
            let PromptRoute::Local { session_id } = route_in(None, handle, None).unwrap() else {
                panic!("local route")
            };
            let session = PtySessionId::new(session_id);
            for submit in [true, false] {
                let result = if submit {
                    host.submit_line(&session, "hello", 0, 0, 0, 0).map(|_| ())
                } else {
                    host.write(
                        &session,
                        crate::commands::pty::bracketed_paste("hello").as_bytes(),
                    )
                };
                assert!(result.is_err(), "{handle}, submit={submit}: {result:?}");
            }
        }
    }

    #[test]
    fn remote_submitted_is_a_success_naming_the_machine() {
        let result = remote_result(
            &json!({"status":"submitted","submitted":true,"input":"agent"}),
            "codesense-test-1",
        );
        assert_eq!(result.status, "submitted");
        assert_eq!(result.submitted, Some(true));
        assert_eq!(result.machine.as_deref(), Some("codesense-test-1"));
        assert_eq!(result.message, None);
    }

    #[test]
    fn remote_typed_is_a_success_without_submission() {
        let result = remote_result(&json!({"status":"typed","submitted":false}), "box");
        assert_eq!(result.status, "typed");
        assert_eq!(result.submitted, Some(false));
        assert_eq!(result.machine.as_deref(), Some("box"));
        assert_eq!(result.message, None);
    }

    #[test]
    fn remote_unknown_keeps_its_uncertainty() {
        let result = remote_result(
            &json!({"status":"unknown","submitted":null,"code":"deliveryUnknown","message":"lost"}),
            "box",
        );
        assert_eq!(result.status, "unknown");
        assert_eq!(result.submitted, None);
        assert_eq!(result.code.as_deref(), Some("deliveryUnknown"));
    }

    #[test]
    fn a_disconnected_machine_is_named_in_the_refusal() {
        let result = remote_result(
            &json!({"status":"refused","submitted":false,"code":"staleOrigin","message":"machine is disconnected"}),
            "codesense-test-1",
        );
        assert_eq!(result.status, "refused");
        assert_eq!(result.submitted, Some(false));
        assert_eq!(
            result.message.as_deref(),
            Some("codesense-test-1 is disconnected")
        );
        let unavailable = remote_result(
            &json!({"status":"refused","code":"staleOrigin","message":"remote runtime unavailable"}),
            "codesense-test-1",
        );
        assert_eq!(
            unavailable.message.as_deref(),
            Some("codesense-test-1 is not connected")
        );
    }

    #[test]
    fn not_sent_and_ended_say_what_happened_on_the_machine() {
        let not_ready = remote_result(
            &json!({"status":"notSent","submitted":false,"code":"notReady"}),
            "box",
        );
        assert_eq!(not_ready.submitted, Some(false));
        assert_eq!(
            not_ready.message.as_deref(),
            Some("the agent on box was not ready for input")
        );
        // What `by_handle_bound` returns for a Run on an exited tab (`input_result`
        // rewrites it to refused).
        let ended = remote_result(
            &json!({"status":"refused","code":"ended","submitted":false}),
            "box",
        );
        assert_eq!(ended.message.as_deref(), Some("its tab on box has ended"));
        assert_eq!(ended.submitted, Some(false));
        let correlation = remote_result(
            &json!({"status":"refused","submitted":false,"code":"correlationUnavailable",
                "message":"Original launch correlation is not currently visible; attach using the original request or handle."}),
            "box",
        );
        assert_eq!(
            correlation.message.as_deref(),
            Some("Original launch correlation is not currently visible; attach using the original request or handle (on box)")
        );
        let coded = remote_result(
            &json!({"status":"refused","submitted":false,"code":"attachedElsewhere"}),
            "box",
        );
        assert_eq!(
            coded.message.as_deref(),
            Some("box refused it (attachedElsewhere)")
        );
        let capital = remote_result(
            &json!({"status":"refused","submitted":false,"message":"Machine is connecting."}),
            "box",
        );
        assert_eq!(capital.message.as_deref(), Some("box is connecting"));
    }

    #[test]
    fn a_result_without_a_status_claims_no_more_than_it_says() {
        let unknown = remote_result(&json!({}), "box");
        assert_eq!(unknown.status, "unknown");
        assert_eq!(unknown.submitted, None);
        let sent = remote_result(&json!({"submitted": true}), "box");
        assert_eq!(sent.status, "submitted");
        assert_eq!(sent.message, None);
        let refused = remote_result(&json!({"submitted": false}), "box");
        assert_eq!(refused.status, "refused");
        assert_eq!(refused.message.as_deref(), Some("box did not take it"));
    }

    fn remote_record(
        handle: &str,
        machine: &str,
        session: &str,
    ) -> crate::commands::pty_layout::RemoteShellRecord {
        serde_json::from_value(json!({
            "handle": handle, "owner_id": 41, "request_id": uuid::Uuid::new_v4(),
            "machine_id": machine, "team_key": "acme:design", "workspace_id": "w",
            "workspace": "acme:design|w", "cwd": "/home/dev", "name": null, "launch": "claude",
            "target": {"member":{"machineId":machine,"slot":"remote"},"admissionRevision":"1",
                "namespace":{"serverId":"s","epoch":"e","generation":"1"}, "session": session}
        }))
        .unwrap()
    }

    /// A registered local agent goes to the session its handle is bound to, even when
    /// the row showed an older one; an unregistered (hand-started) one uses the row's.
    #[test]
    fn a_registered_local_handle_wins_over_the_rows_session() {
        let state = DashboardLaunchState::new(4);
        state.register("barb#1", "sess-now", Some("codesense"));
        assert_eq!(
            route_in(Some(&state), "barb#1", None),
            Ok(PromptRoute::Local {
                session_id: "sess-now".into()
            })
        );
        assert_eq!(
            route_in(Some(&state), "barb#1", Some("sess-stale")),
            Ok(PromptRoute::Local {
                session_id: "sess-now".into()
            })
        );
        assert_eq!(
            route_in(Some(&state), "sess-hand", Some("sess-hand")),
            Ok(PromptRoute::Local {
                session_id: "sess-hand".into()
            })
        );
    }

    /// Resolution order: a local handle and a session no remote tab runs stay on this
    /// Mac even while remote tabs are registered; the remote record of a handle wins
    /// over a local binding of the row's session.
    #[test]
    fn local_and_remote_resolution_order_is_pinned() {
        let state = DashboardLaunchState::new(4);
        state
            .reserve_remote_shell(remote_record("remote-tab-a", "m-1", "remote-shell-a"))
            .unwrap();
        state.register("barb#1", "sess-local", Some("codesense"));
        assert_eq!(
            route_in(Some(&state), "barb#1", Some("sess-local")),
            Ok(PromptRoute::Local {
                session_id: "sess-local".into()
            })
        );
        assert_eq!(
            route_in(Some(&state), "unknown", Some("sess-elsewhere")),
            Ok(PromptRoute::Local {
                session_id: "sess-elsewhere".into()
            })
        );
        assert_eq!(
            route_in(Some(&state), "remote-tab-a", Some("sess-local")),
            Ok(PromptRoute::Remote {
                handle: "remote-tab-a".into(),
                machine_id: "m-1".into()
            })
        );
        assert_eq!(
            route_for(
                None,
                Some("sess-bound".into()),
                pair("remote-tab-a", "m-1"),
                Some("x")
            ),
            Ok(PromptRoute::Local {
                session_id: "sess-bound".into()
            })
        );
    }

    /// The bug: a remote tab the picker lists was looked up among this Mac's sessions.
    #[test]
    fn the_registry_routes_a_listed_remote_tab_to_its_machine() {
        let state = DashboardLaunchState::new(4);
        state
            .reserve_remote_shell(remote_record(
                "remote-tab-f05",
                "m-test-1",
                "remote-shell-f05",
            ))
            .unwrap();
        state.register("barb#1", "sess-local", Some("codesense"));
        let remote = Ok(PromptRoute::Remote {
            handle: "remote-tab-f05".into(),
            machine_id: "m-test-1".into(),
        });
        assert_eq!(
            route_in(Some(&state), "remote-tab-f05", Some("remote-shell-f05")),
            remote
        );
        // A roster row that names the remote tab only by its session.
        assert_eq!(
            route_in(Some(&state), "remote-shell-f05", Some("remote-shell-f05")),
            remote
        );
        assert_eq!(route_in(Some(&state), "remote-shell-f05", None), remote);
        assert_eq!(
            route_in(Some(&state), "barb#1", Some("sess-local")),
            Ok(PromptRoute::Local {
                session_id: "sess-local".into()
            })
        );
        assert_eq!(
            route_in(None, "barb#1", Some("sess-local")),
            Ok(PromptRoute::Local {
                session_id: "sess-local".into()
            })
        );
    }

    #[test]
    fn local_outcome_reports_whether_the_agent_took_it() {
        assert_eq!(local_result(true).status, "submitted");
        let typed = local_result(false);
        assert_eq!(typed.status, "typed");
        assert_eq!(typed.submitted, Some(false));
        assert_eq!(typed.machine, None);
    }

    #[test]
    fn the_result_serializes_in_camel_case_with_nulls() {
        let value = serde_json::to_value(local_result(true)).unwrap();
        assert_eq!(
            value,
            json!({"status":"submitted","submitted":true,"machine":null,"code":null,"message":null})
        );
    }
}
