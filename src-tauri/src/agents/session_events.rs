// SPDX-License-Identifier: MIT
//! Agent-done intake: the one subscriber that turns an agent session's
//! `signal_done` on the dashboard stream into work.
//!
//! Every `signal_done` is offered to routines first, with no owning-team
//! resolution and no provenance filter: a routine's run has neither an
//! automation id nor a story handle, and routine admission applies its own loop
//! guard. It then goes to every consumer registered with
//! [`register_agent_done_consumer`], in registration order. A consumer runs on
//! the receive loop, so anything slow must be spawned detached; stalling would
//! let the broadcast lag and drop later signals.

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::broadcast;

use crate::dashboard_stream::hub::StreamEvent;
use crate::dashboard_stream::{DashboardStreamHub, StatusDelta};
use crate::shared::panic_guard::guarded_wake;

/// Called with the app and the finished session's id.
pub type AgentDoneConsumer<R> = Arc<dyn Fn(&AppHandle<R>, &str) + Send + Sync>;

/// The consumers fed after routines, in registration order.
pub struct AgentDoneConsumers<R: Runtime>(Mutex<Vec<AgentDoneConsumer<R>>>);

impl<R: Runtime> Default for AgentDoneConsumers<R> {
    fn default() -> Self {
        Self(Mutex::new(Vec::new()))
    }
}

impl<R: Runtime> AgentDoneConsumers<R> {
    fn snapshot(&self) -> Vec<AgentDoneConsumer<R>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The app's consumer list, registered on first use.
fn consumers<R: Runtime>(app: &AppHandle<R>) -> tauri::State<'_, AgentDoneConsumers<R>> {
    if app.try_state::<AgentDoneConsumers<R>>().is_none() {
        app.manage(AgentDoneConsumers::<R>::default());
    }
    app.state::<AgentDoneConsumers<R>>()
}

/// Feed every `signal_done` delivered from now on to `consumer`, after
/// routines. Register before the producer starts so no early signal misses it.
pub fn register_agent_done_consumer<R: Runtime>(
    app: &AppHandle<R>,
    consumer: impl Fn(&AppHandle<R>, &str) + Send + Sync + 'static,
) {
    consumers(app)
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Arc::new(consumer));
}

/// Subscribe to the app's dashboard stream and deliver every `signal_done`, in
/// order, from one async task: routines first, then each consumer. A panic in
/// any one delivery is logged and swallowed so it never ends the task. A
/// `Lagged` broadcast drops that many signals (best effort under burst); a
/// `Closed` channel ends the task (app teardown).
pub fn spawn_session_status_producer<R: Runtime>(app: AppHandle<R>) {
    let Some(hub) = app.try_state::<DashboardStreamHub>() else {
        log::warn!(
            target: "agents",
            "no dashboard stream hub in state; agent-done intake not started"
        );
        return;
    };
    consumers(&app);
    let mut rx = hub.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(StreamEvent::Session {
                    delta:
                        StatusDelta::SessionStatus {
                            session_id,
                            status,
                            summary,
                        },
                }) => {
                    guarded_wake("agent-done routines", || {
                        crate::routines::event::offer_agent_done(&app, &session_id, status, summary)
                    });
                    for consumer in consumers(&app).snapshot() {
                        guarded_wake("agent-done consumer", || consumer(&app, &session_id));
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    log::warn!(
                        target: "agents",
                        "agent-done intake lagged; {n} dropped signal(s) will not be delivered"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn app_with_hub() -> tauri::App<tauri::test::MockRuntime> {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(DashboardStreamHub::new());
        app
    }

    fn wait_for(done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn registered_consumers_get_each_signal_in_order_past_a_panicking_one() {
        let app = app_with_hub();
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        register_agent_done_consumer(app.handle(), |_, _| panic!("a broken consumer"));
        let first = Arc::clone(&seen);
        register_agent_done_consumer(app.handle(), move |_, session| {
            first.lock().unwrap().push(format!("first:{session}"));
        });
        spawn_session_status_producer(app.handle().clone());
        let second = Arc::clone(&seen);
        register_agent_done_consumer(app.handle(), move |_, session| {
            second.lock().unwrap().push(format!("second:{session}"));
        });

        app.state::<DashboardStreamHub>().on_session_status(
            "sess-1",
            "success",
            Some("done".into()),
        );
        wait_for(|| seen.lock().unwrap().len() == 2);
        app.state::<DashboardStreamHub>()
            .on_session_status("sess-2", "success", None);
        wait_for(|| seen.lock().unwrap().len() == 4);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                "first:sess-1",
                "second:sess-1",
                "first:sess-2",
                "second:sess-2"
            ]
        );
    }
}
