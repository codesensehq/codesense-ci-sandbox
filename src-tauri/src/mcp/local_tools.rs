// SPDX-License-Identifier: MIT
//! Local inbox recovery follows initialized MCP connections, independently of the
//! terminal transport. Publish snapshots on lifecycle events and terminal reconnect.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use codesense_pty_core::domain::PtySessionId;
use tauri::{AppHandle, Manager};

type Publish = Arc<dyn Fn(Vec<PtySessionId>) + Send + Sync>;

#[derive(Default)]
pub(crate) struct Connections(Mutex<BTreeMap<String, usize>>);

impl Connections {
    fn change(&self, session: &str, connected: bool, publish: &Publish) {
        let mut sessions = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if connected {
            *sessions.entry(session.into()).or_default() += 1;
        } else if let Some(count) = sessions.get_mut(session) {
            *count -= 1;
            if *count == 0 {
                sessions.remove(session);
            }
        }
        // Keep publication ordered with changes and reconnect snapshots.
        publish(sessions.keys().map(PtySessionId::new).collect());
    }

    pub(crate) fn connect(self: &Arc<Self>, session: String, publish: Publish) -> Connection {
        self.change(&session, true, &publish);
        Connection {
            connections: self.clone(),
            session,
            publish,
        }
    }
}

pub(crate) struct Connection {
    connections: Arc<Connections>,
    session: String,
    publish: Publish,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.connections.change(&self.session, false, &self.publish);
    }
}

#[derive(Default)]
pub(crate) struct ConnectionState {
    pub(crate) connection: Option<Connection>,
    pub(crate) closed: bool,
}

/// Ends readiness with the transport, even if an in-flight tool retains a handler.
pub(crate) struct Lifetime(pub(crate) Arc<Mutex<ConnectionState>>);
impl Lifetime {
    pub(crate) fn close(slot: &Arc<Mutex<ConnectionState>>) {
        let mut slot = slot.lock().unwrap_or_else(|p| p.into_inner());
        slot.closed = true;
        slot.connection.take();
    }
}
impl Drop for Lifetime {
    fn drop(&mut self) {
        Self::close(&self.0);
    }
}

/// Re-resolve the live client on every snapshot, including reconnect publication.
pub(crate) fn publisher_with(
    client: impl Fn() -> Option<Arc<crate::terminal::session_server::SessionServerClient>>
        + Send
        + Sync
        + 'static,
) -> Publish {
    Arc::new(move |sessions| {
        if let Some(client) = client() {
            client.local_mcp_sessions(sessions);
        }
    })
}

fn publisher(app: &AppHandle) -> Publish {
    let app = app.clone();
    publisher_with(move || {
        app.try_state::<crate::terminal::session_server::SessionServerState>()
            .and_then(|state| {
                crate::terminal::session_server::SessionServerState::connected(&state)
            })
    })
}

/// The initialized notification's lifecycle action, shared with protocol fixtures.
/// The concrete app-bound handler delegates here so tests need no GUI AppHandle.
pub(crate) fn initialized(
    slot: &Mutex<ConnectionState>,
    session: Option<String>,
    connect: impl FnOnce(String) -> Option<Connection>,
) {
    if let Some(session) = session {
        let mut slot = slot.lock().unwrap_or_else(|p| p.into_inner());
        if !slot.closed && slot.connection.is_none() {
            slot.connection = connect(session);
        }
    }
}

pub(crate) fn connected(app: &AppHandle, session: String) -> Option<Connection> {
    Some(Connections::connect(
        &*app.try_state::<Arc<Connections>>()?,
        session,
        publisher(app),
    ))
}

pub(crate) fn refresh(app: &AppHandle) {
    if let Some(state) = app.try_state::<Arc<Connections>>() {
        let sessions = state.0.lock().unwrap_or_else(|p| p.into_inner());
        publisher(app)(sessions.keys().map(PtySessionId::new).collect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_handler_delegates_initialized_to_the_shared_lifecycle() {
        let source = include_str!("handler.rs");
        let callback = source
            .split("async fn on_initialized(")
            .nth(1)
            .unwrap()
            .split("async fn call_tool(")
            .next()
            .unwrap();
        assert!(callback.contains("super::local_tools::initialized("));
    }

    #[test]
    fn transport_lifetime_publishes_per_session_mcp_readiness_and_retains_other_links() {
        let connections = Arc::new(Connections::default());
        let snapshots = Arc::new(Mutex::new(Vec::new()));
        let recorded = snapshots.clone();
        let publish: Publish = Arc::new(move |sessions| recorded.lock().unwrap().push(sessions));
        let slot = Arc::new(Mutex::new(ConnectionState {
            connection: Some(connections.connect("s".into(), publish.clone())),
            closed: false,
        }));
        let other = connections.connect("s".into(), publish.clone());
        let separate = connections.connect("other".into(), publish);
        drop(Lifetime(slot.clone()));
        assert!(slot.lock().unwrap().connection.is_none());
        assert!(slot.lock().unwrap().closed);
        assert_eq!(
            snapshots.lock().unwrap().last().unwrap(),
            &[PtySessionId::new("other"), PtySessionId::new("s")]
        );
        drop(other);
        assert_eq!(
            snapshots.lock().unwrap().last().unwrap(),
            &[PtySessionId::new("other")]
        );
        drop(separate);
        assert!(snapshots.lock().unwrap().last().unwrap().is_empty());
    }
}
