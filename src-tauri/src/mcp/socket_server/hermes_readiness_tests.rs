// SPDX-License-Identifier: MIT
//! A Hermes tab's inbox wake needs its CodeSense tools actually loaded, on both sides: the
//! gateway reports its MCP server connected with tools, and CodeSense has seen the MCP
//! connection initialize. Both come from real rmcp traffic: the gateway's report is
//! derived from what the served connection did, and the host learns of the connection
//! through the production `initialized` action and publisher, a live
//! `SessionServerClient`, the host's `LocalMcpSessions` receiver and its real `HostInbox`.
use super::tests::{Client, INITIALIZE};
use super::*;
use crate::mcp::local_tools::{self, ConnectionState, Connections, Lifetime};
use crate::terminal::session_server::SessionServerClient;
use codesense_pty_core::activity::Exit;
use codesense_pty_core::agent_cli::hermes::gateway_fixture::{GatewayFixture, McpServer};
use codesense_pty_core::agent_cli::AgentCli;
use codesense_pty_core::domain::{
    PtySessionId, TerminalDomain, TerminalDomainEvent, TerminalEventSink,
};
use codesense_pty_core::inbox_host::Fate;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Every wait below is on an event; this only bounds a test whose event never comes.
const BOUND: Duration = Duration::from_secs(10);
/// Past the host's inbox sweep interval, so a sweep runs now.
const SWEEP: Duration = Duration::from_secs(6);
const SESSION: &str = "hermes-ac5";
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
const TOOLS_LIST: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

/// What the served MCP connection did, which is all a gateway can know of it.
#[derive(Default)]
struct Traffic {
    initialized: bool,
    tools: u64,
}

/// The MCP side of a managed tab: the production lifecycle action and publisher, over a
/// protocol served by rmcp. Only the app-bound handler is replaced (it needs a GUI
/// `AppHandle`); a test asserts the real handler delegates to the same action.
struct Bridge {
    slot: Arc<Mutex<ConnectionState>>,
    connections: Arc<Connections>,
    client: Arc<SessionServerClient>,
    traffic: Arc<Mutex<Traffic>>,
    initialized: Sender<()>,
}

impl rmcp::ServerHandler for Bridge {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::InitializeResult::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
    }

    async fn list_tools(
        &self,
        _: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        let tools = crate::mcp::handler::CodesenseHandler::served_tool_router().list_all();
        self.traffic.lock().unwrap().tools = tools.len() as u64;
        Ok(rmcp::model::ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        })
    }

    async fn on_initialized(&self, _: rmcp::service::NotificationContext<rmcp::RoleServer>) {
        self.traffic.lock().unwrap().initialized = true;
        let client = self.client.clone();
        local_tools::initialized(&self.slot, Some(SESSION.into()), |session| {
            Some(self.connections.connect(
                session,
                local_tools::publisher_with(move || Some(client.clone())),
            ))
        });
        let _ = self.initialized.send(());
    }
}

/// The desktop's view of its host: every event the host pushes.
struct Events(Mutex<Sender<TerminalDomainEvent>>);

impl TerminalEventSink for Events {
    fn emit(
        &self,
        event: TerminalDomainEvent,
    ) -> Result<(), codesense_pty_core::domain::PortUnavailable> {
        let _ = self.0.lock().unwrap().send(event);
        Ok(())
    }
}

fn next<T>(
    events: &Receiver<TerminalDomainEvent>,
    what: &str,
    mut pick: impl FnMut(TerminalDomainEvent) -> Option<T>,
) -> T {
    let until = std::time::Instant::now() + BOUND;
    loop {
        let left = until.saturating_duration_since(std::time::Instant::now());
        let event = events
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("the host never reported {what}"));
        if let Some(found) = pick(event) {
            return found;
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Handshake {
    /// initialize, `notifications/initialized`, `tools/list`.
    Complete,
    /// initialize only: the client never confirms, so rmcp serves nothing further.
    NeverInitialized,
    /// Complete, then the transport ends.
    EndsAfterwards,
}

#[derive(Debug)]
struct Case {
    /// Launched by CodeSense with its engine and hooks; a hand-typed Hermes is not.
    managed: bool,
    traffic: Handshake,
    /// The gateway is handed the notice through its engine.
    delivered: bool,
}

#[test]
fn a_hermes_notice_reaches_the_gateway_only_while_its_tools_are_loaded_and_published() {
    let cases = [
        Case {
            managed: true,
            traffic: Handshake::Complete,
            delivered: true,
        },
        Case {
            managed: false,
            traffic: Handshake::Complete,
            delivered: false,
        },
        Case {
            managed: true,
            traffic: Handshake::NeverInitialized,
            delivered: false,
        },
        Case {
            managed: true,
            traffic: Handshake::EndsAfterwards,
            delivered: false,
        },
    ];
    for case in cases {
        run(&case);
    }
}

fn run(case: &Case) {
    let at = format!("{case:?}");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let root = codesense_session_server::testing::short_root();
    let server = codesense_session_server::testing::TestServer::start_at(root.path());
    server.host().start_activity(root.path()).unwrap();
    let (sent, events) = channel();
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(Events(Mutex::new(sent))),
        Arc::default(),
        None,
    )
    .unwrap();
    // The app opts in to the host's activity pushes as it connects.
    assert!(client.activity_snapshot().unwrap().is_empty());
    let owner = server.host().terminal().activity().clone();
    let session = PtySessionId::new(SESSION);
    // This process stands in for the Hermes CLI, which owns the engine.
    let writer = case.managed.then(|| {
        owner
            .prepare_native(SESSION, AgentCli::Hermes, "unused")
            .unwrap()
    });
    let incarnation = owner.register(SESSION, Some(std::process::id())).unwrap();

    // The gateway knows of its MCP server only what the served connection did.
    let traffic = Arc::new(Mutex::new(Traffic::default()));
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let endpoint = owner.engine_socket_for_test("hermes").unwrap();
    let seen = traffic.clone();
    let gateway = GatewayFixture::start(home.path(), &endpoint, move || {
        let seen = seen.lock().unwrap();
        McpServer {
            connected: seen.initialized,
            tools: seen.tools,
        }
    });

    let slot = Arc::new(Mutex::new(ConnectionState::default()));
    // A tool call in flight keeps the handler, and so its slot, past the transport's end.
    let _retained_by_a_tool_call = slot.clone();
    let (ready, initialized) = channel();
    let bridge = Bridge {
        slot: slot.clone(),
        connections: Arc::default(),
        client: client.clone(),
        traffic,
        initialized: ready,
    };
    let (pipe, served) = tokio::io::duplex(64 * 1024);
    let serving = runtime.spawn(serve_with_local_tools(bridge, served, Some(Lifetime(slot))));
    let mut mcp = Client::over(pipe);
    runtime.block_on(async {
        mcp.send(INITIALIZE).await;
        assert_eq!(mcp.answer().await["id"], 0);
        if !matches!(case.traffic, Handshake::NeverInitialized) {
            mcp.send(INITIALIZED).await;
            mcp.send(TOOLS_LIST).await;
            let listed = mcp.answer().await;
            assert!(
                listed["result"]["tools"]
                    .as_array()
                    .is_some_and(|t| !t.is_empty()),
                "{listed}"
            );
        }
    });
    if !matches!(case.traffic, Handshake::NeverInitialized) {
        initialized.recv_timeout(BOUND).expect("on_initialized ran");
    }

    // The engine comes up with the tools state the traffic left it in.
    if let Some(writer) = &writer {
        owner
            .bind_engine_with_home_for_test(writer, endpoint, Some(home.path().to_path_buf()))
            .unwrap();
    }
    let engine_ready = case.managed
        && matches!(
            case.traffic,
            Handshake::Complete | Handshake::EndsAfterwards
        );
    if engine_ready {
        next(&events, "the engine's readiness", |event| match event {
            TerminalDomainEvent::Activity { snapshot } if snapshot.observed => Some(()),
            _ => None,
        });
        assert!(owner.snapshot(SESSION).unwrap().takes_input(), "{at}");
    }
    // The host's own sweep, as it runs every few seconds: whom it submits to, and so what
    // the desktop is told of the agent's inboxWaiter, before any mail is pushed. A call on
    // the connection that carried the publications answers only after the host has taken
    // each of them, so the sweep sees what the desktop last said. Each sweep is a later
    // instant than the one before.
    let sweeps = std::cell::Cell::new(0u32);
    let waiting = |client: &SessionServerClient| {
        client.inbox_waiters().unwrap();
        sweeps.set(sweeps.get() + 1);
        owner.decay(std::time::Instant::now() + SWEEP * sweeps.get());
        client
            .inbox_waiters()
            .unwrap()
            .iter()
            .any(|w| w.session == session && w.waiting)
    };
    if matches!(case.traffic, Handshake::EndsAfterwards) {
        assert!(
            waiting(&client),
            "{at}: it is reached before its transport ends"
        );
        drop(mcp);
        runtime.block_on(serving).unwrap();
    }
    assert_eq!(waiting(&client), case.delivered, "{at}: inboxWaiter");

    client
        .inbox_mail(&session, "You have 1 new message.", "tok-1")
        .unwrap();
    if case.delivered {
        let fate = next(&events, "the notice's fate", |event| Fate::of(event));
        assert_eq!(
            (fate.1.as_str(), fate.2),
            ("tok-1", Fate::Uncertain),
            "{at}"
        );
        let prompts = gateway.observed().prompts().len();
        assert_eq!(prompts, 1, "{at}: the gateway was handed the notice once");
    } else {
        // Nothing is submitted: the host cannot tell the agent has its tools.
        assert!(owner.inbox().holds(SESSION), "{at}: the mail stays held");
        assert!(gateway.observed().prompts().is_empty(), "{at}");
        while let Ok(event) = events.try_recv() {
            assert!(Fate::of(event).is_none(), "{at}: no notice was settled");
        }
        // The agent exits: no notice is held for it, and no one is waiting.
        owner.exit(
            SESSION,
            &incarnation,
            Exit {
                code: Some(0),
                signal: None,
                requested: true,
            },
        );
        assert!(
            !owner.inbox().holds(SESSION),
            "{at}: exit drops the held mail"
        );
        assert!(!waiting(&client), "{at}: exit clears the waiter");
    }
}
