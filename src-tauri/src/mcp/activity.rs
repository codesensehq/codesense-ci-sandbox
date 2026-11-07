// SPDX-License-Identifier: MIT
//! Captured control-plane activity writes. Business effects retain their owners;
//! delayed completion can never select a newer invocation or turn.
use crate::commands::pty_backend::ResolvedTerminalBackend;
use codesense_pty_core::activity::{Action, Identity, Mutation, Outcome, Snapshot, State};
use codesense_pty_core::agent_cli::AgentCli;
use tauri::{AppHandle, Emitter, Runtime};

#[derive(Clone)]
pub(crate) enum Capture {
    Legacy,
    Unavailable,
    Owner {
        backend: Box<ResolvedTerminalBackend>,
        session: String,
        identity: Identity,
        turn_generation: u64,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    Legacy,
    Applied,
    Unavailable,
}

impl Delivery {
    pub(crate) fn as_wire(self) -> &'static str {
        match self {
            Self::Legacy => "limited",
            Self::Applied => "applied",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompletionBoundary {
    pub(crate) identity: Identity,
    pub(crate) turn_generation: u64,
    pub(crate) revision: u64,
}

pub(crate) struct WriteResult {
    pub(crate) delivery: Delivery,
    completion: Option<CompletionBoundary>,
}

impl WriteResult {
    fn without_boundary(delivery: Delivery) -> Self {
        Self {
            delivery,
            completion: None,
        }
    }

    /// Emit once even when owner delivery failed. Only the successful mutation's
    /// captured result can associate the task finish with a host turn.
    pub(crate) fn emit_done<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        session: &str,
        status: &str,
        summary: &Option<String>,
    ) -> tauri::Result<()> {
        let mut payload = serde_json::json!({
            "sessionId": session,
            "status": status,
            "summary": summary,
        });
        if let Some(completion) = &self.completion {
            payload["activityCompletion"] = serde_json::to_value(completion)?;
        }
        app.emit("mcp:agent-done", payload)
    }
}

/// The caller supplies a fresh operation id and never retries it: an owner's
/// duplicate-operation reply describes its current state, not the original commit.
fn write_owner<E>(
    mutation: Mutation,
    mut revalidate: impl FnMut() -> bool,
    mutate: impl FnOnce(Mutation) -> Result<Snapshot, E>,
) -> WriteResult {
    if !revalidate() {
        return WriteResult::without_boundary(Delivery::Unavailable);
    }
    let expected = mutation.clone();
    let Ok(snapshot) = mutate(mutation) else {
        return WriteResult::without_boundary(Delivery::Unavailable);
    };
    if !revalidate() {
        return WriteResult::without_boundary(Delivery::Unavailable);
    }
    let completion = match expected.action {
        Action::Done { outcome } => {
            let state = match outcome {
                Outcome::Success => State::Done,
                Outcome::Failed => State::Errored,
                Outcome::NeedsAttention => State::Done,
            };
            (snapshot.session == expected.session
                && snapshot.identity == expected.identity
                && snapshot.turn_generation == expected.turn_generation
                && snapshot.state == state
                && snapshot.exit.is_none())
            .then_some(CompletionBoundary {
                identity: snapshot.identity,
                turn_generation: snapshot.turn_generation,
                revision: snapshot.revision,
            })
        }
        _ => None,
    };
    WriteResult {
        delivery: Delivery::Applied,
        completion,
    }
}

/// `session`'s live agent as its activity owner reports it, and the backend read
/// (revalidated after the read); otherwise the capture that stands for it: an owner too
/// old to report, or no live agent, is `Legacy`.
fn live_snapshot(
    app: &AppHandle,
    session: &str,
) -> Result<(ResolvedTerminalBackend, Snapshot), Capture> {
    use crate::commands::machines::RuntimeSessionReference;
    let backend = crate::commands::pty_backend::resolve_session_reference(
        app,
        &RuntimeSessionReference::Legacy(session.into()),
        false,
    )
    .map_err(|_| Capture::Unavailable)?;
    let snapshots = match backend.domain().activity_snapshot() {
        Ok(snapshots) => snapshots,
        Err(error) if error.to_string().contains("upgrade required") => {
            return Err(Capture::Legacy)
        }
        Err(_) => return Err(Capture::Unavailable),
    };
    backend.revalidate(app).map_err(|_| Capture::Unavailable)?;
    let snapshot = snapshots
        .into_iter()
        .find(|snapshot| snapshot.session == session && snapshot.exit.is_none())
        .ok_or(Capture::Legacy)?;
    Ok((backend, snapshot))
}

pub(crate) fn capture_blocking(app: &AppHandle, session: &str) -> Capture {
    match live_snapshot(app, session) {
        Ok((backend, snapshot)) => Capture::Owner {
            backend: Box::new(backend),
            session: session.into(),
            identity: snapshot.identity,
            turn_generation: snapshot.turn_generation,
        },
        Err(capture) => capture,
    }
}

/// The CLI `session` runs now, as its activity owner knows it: `None` for a shell, an
/// agent that exited, or a session whose owner cannot be read.
pub(crate) fn session_cli(app: &AppHandle, session: &str) -> Option<AgentCli> {
    live_snapshot(app, session)
        .ok()
        .map(|(_, snapshot)| snapshot.provider)
}

pub(crate) async fn capture(app: &AppHandle, session: &str) -> Capture {
    let app = app.clone();
    let session = session.to_owned();
    crate::commands::await_blocking("activity.capture", move || {
        Ok(capture_blocking(&app, &session))
    })
    .await
    .unwrap_or(Capture::Unavailable)
}

impl Capture {
    /// Read through the captured owning backend, without selecting a replacement invocation.
    pub(crate) async fn session_output(
        &self,
        app: &AppHandle,
    ) -> Option<codesense_pty_core::agent_cli::output::SessionRecord> {
        let Self::Owner {
            backend,
            session,
            identity,
            turn_generation,
        } = self.clone()
        else {
            return None;
        };
        let app = app.clone();
        crate::commands::await_blocking("activity.output", move || {
            crate::commands::pty_backend::ResolvedTerminalBackend::revalidate(&backend, &app)?;
            let record = backend
                .domain()
                .session_output(
                    &codesense_pty_core::domain::PtySessionId::new(session),
                    &identity,
                    turn_generation,
                )
                .map_err(|e| e.to_string())?;
            crate::commands::pty_backend::ResolvedTerminalBackend::revalidate(&backend, &app)?;
            Ok(record)
        })
        .await
        .ok()
        .flatten()
    }

    pub(crate) fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy)
    }

    pub(crate) fn write_blocking(
        &self,
        app: &AppHandle,
        action: Action,
        operation_id: String,
    ) -> Delivery {
        self.write_result_blocking(app, action, operation_id)
            .delivery
    }

    fn write_result_blocking(
        &self,
        app: &AppHandle,
        action: Action,
        operation_id: String,
    ) -> WriteResult {
        match self {
            Self::Legacy => WriteResult::without_boundary(Delivery::Legacy),
            Self::Unavailable => WriteResult::without_boundary(Delivery::Unavailable),
            Self::Owner {
                backend,
                session,
                identity,
                turn_generation,
            } => {
                let mutation = Mutation {
                    session: session.clone(),
                    identity: identity.clone(),
                    turn_generation: *turn_generation,
                    operation_id,
                    action,
                };
                write_owner(
                    mutation,
                    || backend.revalidate(app).is_ok(),
                    |mutation| backend.domain().activity_mutate(mutation),
                )
            }
        }
    }

    pub(crate) async fn write(&self, app: &AppHandle, action: Action) -> Delivery {
        self.write_result(app, action).await.delivery
    }

    pub(crate) async fn write_done(&self, app: &AppHandle, outcome: Outcome) -> WriteResult {
        self.write_result(app, Action::Done { outcome }).await
    }

    async fn write_result(&self, app: &AppHandle, action: Action) -> WriteResult {
        let capture = self.clone();
        let app = app.clone();
        let operation_id = uuid::Uuid::new_v4().to_string();
        crate::commands::await_blocking("activity.mutate", move || {
            Ok(capture.write_result_blocking(&app, action, operation_id))
        })
        .await
        .unwrap_or_else(|_| WriteResult::without_boundary(Delivery::Unavailable))
    }
}

/// Cleanup ownership exists before opening can commit. A cancelled open still
/// closes its exact ID; the host retires that ID even if close arrives first.
pub(crate) struct AskGuard {
    app: AppHandle,
    capture: Capture,
    id: String,
    settled: bool,
}

impl AskGuard {
    pub(crate) async fn open(app: &AppHandle, capture: Capture, id: &str) -> (Self, Delivery) {
        let guard = Self {
            app: app.clone(),
            capture,
            id: id.into(),
            settled: false,
        };
        let delivery = guard
            .capture
            .write(app, Action::AskOpened { id: id.into() })
            .await;
        (guard, delivery)
    }

    pub(crate) async fn settle(mut self, resolved: bool) -> Delivery {
        let delivery = self
            .capture
            .write(
                &self.app,
                Action::AskClosed {
                    id: self.id.clone(),
                    resolved,
                },
            )
            .await;
        self.settled = true;
        delivery
    }
}

impl Drop for AskGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let capture = self.capture.clone();
        let app = self.app.clone();
        let id = self.id.clone();
        // IPC may wait for a disconnected host. Never block the async handler's
        // executor thread in Drop; the domain retains its existing RPC bound.
        tauri::async_runtime::spawn_blocking(move || {
            capture.write_blocking(
                &app,
                Action::AskClosed {
                    id,
                    resolved: false,
                },
                uuid::Uuid::new_v4().to_string(),
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesense_pty_core::agent_cli::AgentCli;
    use std::sync::{Arc, Mutex};
    use tauri::Listener;

    fn mutation(outcome: Outcome) -> Mutation {
        Mutation {
            session: "session".into(),
            identity: Identity {
                host_boot_id: "boot".into(),
                session_incarnation: "pty".into(),
                invocation_id: "invocation".into(),
            },
            turn_generation: 7,
            operation_id: uuid::Uuid::new_v4().to_string(),
            action: Action::Done { outcome },
        }
    }

    fn snapshot(mutation: &Mutation, state: State) -> Snapshot {
        Snapshot {
            session: mutation.session.clone(),
            identity: mutation.identity.clone(),
            revision: 31,
            turn_generation: mutation.turn_generation,
            provider: AgentCli::Claude,
            state,
            observed: true,
            limited: false,
            exit: None,
            asks: vec![],
            reason: None,
            tool_open: Some(false),
            activity_age_ms: Some(0),
            subagents_open: 0,
        }
    }

    fn emitted(result: &WriteResult, status: &str) -> serde_json::Value {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let received = seen.clone();
        app.listen("mcp:agent-done", move |event| {
            received
                .lock()
                .unwrap()
                .push(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
        });
        result
            .emit_done(app.handle(), "session", status, &Some("finished".into()))
            .unwrap();
        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["sessionId"], "session");
        assert_eq!(events[0]["status"], status);
        assert_eq!(events[0]["summary"], "finished");
        events[0].clone()
    }

    #[test]
    fn completion_event_carries_the_exact_committed_turn_for_each_outcome() {
        for (outcome, state, status) in [
            (Outcome::Success, State::Done, "success"),
            (Outcome::Failed, State::Errored, "failed"),
            (Outcome::NeedsAttention, State::Done, "needs-attention"),
        ] {
            let mutation = mutation(outcome);
            let committed = snapshot(&mutation, state);
            let result = write_owner(mutation, || true, |_| Ok::<_, &str>(committed.clone()));
            assert!(result.delivery == Delivery::Applied);
            // Later owner state is deliberately never queried or used to tag the event.
            let event = emitted(&result, status);
            assert_eq!(
                event["activityCompletion"],
                serde_json::json!({
                    "identity": committed.identity,
                    "turnGeneration": 7,
                    "revision": 31,
                })
            );
        }
    }

    #[test]
    fn rejected_failed_timed_out_and_unavailable_writes_still_emit_once_without_a_boundary() {
        for error in [
            "superseded activity turn",
            "retired activity invocation",
            "host unavailable",
            "request timed out",
        ] {
            let result = write_owner(
                mutation(Outcome::Success),
                || true,
                |_| Err::<Snapshot, _>(error),
            );
            assert!(result.delivery == Delivery::Unavailable);
            assert!(emitted(&result, "success")
                .get("activityCompletion")
                .is_none());
        }
        for delivery in [Delivery::Legacy, Delivery::Unavailable] {
            assert!(emitted(&WriteResult::without_boundary(delivery), "failed")
                .get("activityCompletion")
                .is_none());
        }
    }

    #[test]
    fn revoked_authority_before_or_after_mutation_never_tags_completion() {
        let mutation = mutation(Outcome::Success);
        let result = write_owner(
            mutation.clone(),
            || false,
            |_| -> Result<Snapshot, &str> { panic!("revoked backend must not receive a write") },
        );
        assert!(result.delivery == Delivery::Unavailable);
        assert!(emitted(&result, "success")
            .get("activityCompletion")
            .is_none());
        let mut checks = [true, false].into_iter();
        let committed = snapshot(&mutation, State::Done);
        let result = write_owner(
            mutation,
            || checks.next().unwrap(),
            |_| Ok::<_, &str>(committed),
        );
        assert!(result.delivery == Delivery::Unavailable);
        assert!(emitted(&result, "success")
            .get("activityCompletion")
            .is_none());
    }

    #[test]
    fn mismatched_mutation_results_and_non_completion_writes_never_supply_a_boundary() {
        for change in 0..6 {
            let mutation = mutation(Outcome::Success);
            let mut returned = snapshot(&mutation, State::Done);
            match change {
                0 => returned.session = "other-session".into(),
                1 => returned.identity.host_boot_id = "other-boot".into(),
                2 => returned.identity.invocation_id = "other-invocation".into(),
                3 => returned.turn_generation += 1,
                4 => returned.state = State::Thinking,
                _ => {
                    returned.exit = Some(codesense_pty_core::activity::Exit {
                        code: Some(0),
                        signal: None,
                        requested: false,
                    })
                }
            }
            let result = write_owner(mutation, || true, |_| Ok::<_, &str>(returned));
            assert!(emitted(&result, "success")
                .get("activityCompletion")
                .is_none());
        }
        let mut mutation = mutation(Outcome::Success);
        mutation.action = Action::Progress { blocked: false };
        let returned = snapshot(&mutation, State::Done);
        let result = write_owner(mutation, || true, |_| Ok::<_, &str>(returned));
        assert!(result.completion.is_none());
    }
}
