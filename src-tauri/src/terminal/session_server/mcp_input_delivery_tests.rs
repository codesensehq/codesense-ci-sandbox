// SPDX-License-Identifier: MIT
struct DisconnectOnSubmitted {
    inner: BufReader<Box<dyn TransportReader>>,
    ready: VecDeque<u8>,
    observed: Arc<AtomicBool>,
}
impl TransportReader for DisconnectOnSubmitted {}
impl Read for DisconnectOnSubmitted {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        while self.ready.is_empty() {
            let Some(frame) = read_frame(&mut self.inner).map_err(std::io::Error::other)? else {
                return Ok(0);
            };
            if matches!(
                frame.decode_json::<Message>(),
                Ok(Message::Reply {
                    result: Ok(Reply::Flag { value: true }),
                    ..
                })
            ) {
                self.observed.store(true, Ordering::SeqCst);
                return Ok(0);
            }
            let mut bytes = Vec::new();
            write_frame(&mut bytes, &frame).map_err(std::io::Error::other)?;
            self.ready.extend(bytes);
        }
        let count = output.len().min(self.ready.len());
        for byte in &mut output[..count] {
            *byte = self.ready.pop_front().unwrap();
        }
        Ok(count)
    }
}

#[test]
fn remote_mcp_lost_input_ack_is_unknown_and_never_replayed() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let admitted = handshake(root.path()).unwrap();
    let observed = Arc::new(AtomicBool::new(false));
    let transport = SessionServerTransport {
        reader: BufReader::new(Box::new(DisconnectOnSubmitted {
            inner: admitted.transport.reader,
            ready: VecDeque::new(),
            observed: observed.clone(),
        })),
        ..admitted.transport
    };
    let client = SessionServerClient::from_admitted_transport(
        root.path(),
        AdmittedSessionTransport {
            transport,
            ..admitted
        },
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let session = sid("mcp-delivery");
    sidecar.track(session.as_str());
    client
        .spawn(
            SpawnIntent {
                remote_launch: Default::default(),
                command: None,
                argv: None,
                shell: Some("/bin/sh".into()),
                cwd: "/tmp".into(),
                cols: 100,
                rows: 30,
                shell_integration: false,
                starship_preset: None,
                api_port: None,
                env_remove: vec![],
                env: vec![],
                agent_env: Vec::new(),
                login_path: None,
            },
            registration(session.as_str(), None, false),
        )
        .unwrap();
    client.claim_writable_typed(&session).unwrap();
    let result =
        client.submit_shell_line_typed(&session, "printf 'ONCE_%s\\n' DELIVERED", 20, 2000);
    assert!(
        matches!(result, Err(InputDeliveryError::Unknown(_))),
        "{result:?}"
    );
    assert!(observed.load(Ordering::SeqCst));
    let reader = SessionServerState::default()
        .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    let text = || {
        reader
            .read_terminal_text(
                &session,
                u32::MAX,
                codesense_pty_core::emulator::TextMode::Normal,
            )
            .unwrap()
            .text
    };
    assert!(wait_until(|| text().contains("ONCE_DELIVERED")));
    let output = text();
    assert_eq!(output.matches("ONCE_DELIVERED").count(), 1, "{output}");
    assert!(matches!(
        client.submit_shell_line_typed(&session, "must-not-send", 20, 100),
        Err(InputDeliveryError::NotSent(_))
    ));
}

#[test]
fn remote_mcp_pending_launch_without_layout_is_not_ended() {
    use crate::commands::machines::{
        RuntimeCounter, RuntimeMemberAddress, RuntimeMemberTarget, RuntimeSessionTarget,
    };
    use crate::commands::pty_layout::RemoteShellRecord;
    use crate::mcp::remote_terminal::{execute, RemoteOperation};
    let server = TestServer::start();
    let client = SessionServerState::default()
        .connect_or_launch(server.root(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    let record = RemoteShellRecord {
        workspace_story_id: None,
        agent: None,
        phase: Default::default(),
        handle: "pending-handle".into(),
        owner_id: 41,
        request_id: uuid::Uuid::new_v4(),
        machine_id: "remote".into(),
        team_key: "org:team".into(),
        workspace_id: "work".into(),
        workspace: crate::commands::pty_layout::workspace_key("org:team", "work"),
        cwd: "/tmp".into(),
        name: None,
        mcp_profile_revision: None,
        mcp_inventory: Vec::new(),
        hooks: false,
        launch: Default::default(),
        permission_mode: Default::default(),
        status: "unknown",
        identity: Default::default(),
        target: RuntimeSessionTarget {
            runtime: RuntimeMemberTarget {
                member: RuntimeMemberAddress::remote("remote"),
                admission_revision: RuntimeCounter::new(1),
                namespace: client.namespace().into(),
            },
            session: sid("pending-original"),
        },
    };
    let read = execute(
        &client,
        &record,
        RemoteOperation::Read {
            since: None,
            screen: false,
        },
        || Ok(()),
    );
    assert_eq!(read["status"], "unavailable");
    assert_eq!(read["code"], "correlationUnavailable");
    assert!(read.get("output").is_none());
    let run = execute(
        &client,
        &record,
        RemoteOperation::Run {
            command: "must-not-run".into(),
            target: crate::mcp::remote_terminal::InputTarget::Auto,
        },
        || Ok(()),
    );
    assert_eq!(run["submitted"], false);
    assert_eq!(run["code"], "correlationUnavailable");
    assert_eq!(record.status, "unknown");
    assert!(client.live_session_ids().unwrap().is_empty());
}

#[test]
fn remote_stdio_missing_capability_refuses_before_send_but_desktop_provision_remains_supported() {
    use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
    use codesense_host_proto::reverse_mcp::{self as wire, RemoteStdioEntry};
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let mut hello = mock_hello(instance_id(root.path()));
    hello.capabilities.extend([
        Capability::ReverseMcp,
        Capability::ReverseMcpServers,
        Capability::NativeRemoteLaunch,
    ]);
    let sent = Arc::new(AtomicUsize::new(0));
    let observed = sent.clone();
    let host = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        greet_client(&stream, &hello, 0);
        while let Some((rid, request)) = next_call(&stream) {
            if matches!(request, Request::Listen) {
                answer(&stream, rid, Ok(Reply::Done));
                continue;
            }
            observed.fetch_add(1, Ordering::SeqCst);
            let Request::PrepareReverseMcp {
                remote_entries,
                server_ids,
                ..
            } = request
            else {
                panic!("unexpected request")
            };
            assert!(remote_entries.is_empty());
            assert!(server_ids.contains(&"desktop-sentinel".into()));
            answer(&stream, rid, Ok(Reply::Done));
            break;
        }
    });
    let mut admitted = handshake(root.path()).unwrap();
    let (lease, _revoke) = crate::commands::auth::private_authority::test_lease(41);
    admitted.transport.private_lease = Some(lease);
    let client = SessionServerClient::from_admitted_transport(
        root.path(),
        admitted,
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let entry = RemoteStdioEntry {
        id: wire::remote_server_id("fixture").unwrap(),
        logical_name: "fixture".into(),
        command: "/remote/server".into(),
        args: vec![],
        cwd: None,
        env: Default::default(),
        env_refs: Default::default(),
    };
    let error = client
        .prepare_native_remote(
            &sid("capability-fixture"),
            "a".repeat(64),
            NativeMcpSelection {
                agent_startup: None,
                server_ids: vec![],
                remote_entries: vec![entry],
            },
            NativeLaunch::Cli(AgentCli::Claude),
            Arc::new(|_, _| {}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("remote stdio MCP unavailable"));
    assert_eq!(sent.load(Ordering::SeqCst), 0);
    let error = client
        .prepare_native_remote(
            &sid("saved-agent-old-daemon"),
            "c".repeat(64),
            NativeMcpSelection {
                server_ids: vec![],
                remote_entries: vec![],
                agent_startup: Some(wire::AgentStartup {
                    args: vec![],
                    settings: serde_json::json!({"permissions":{}}),
                    prompt: None,
                    env: Default::default(),
                }),
            },
            NativeLaunch::Cli(AgentCli::Claude),
            Arc::new(|_, _| {}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("saved-Agent startup"));
    assert_eq!(sent.load(Ordering::SeqCst), 0);
    client
        .prepare_native_remote(
            &sid("capability-fixture"),
            "b".repeat(64),
            NativeMcpSelection {
                agent_startup: None,
                server_ids: vec!["desktop-sentinel".into()],
                remote_entries: vec![],
            },
            NativeLaunch::Cli(AgentCli::Claude),
            Arc::new(|_, _| {}),
        )
        .unwrap();
    host.join().unwrap();
    assert_eq!(sent.load(Ordering::SeqCst), 1);
    client.sever();
}

#[test]
fn remote_native_submit_uses_agent_protocol_and_preserves_unconfirmed_delivery() {
    use crate::commands::mcp_workspace::{
        SUBMIT_CONFIRM_MS, SUBMIT_MAX_NUDGES, SUBMIT_QUIET_MS, SUBMIT_TIMEOUT_MS,
    };
    use crate::commands::pty::InputRoute;
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let hello = mock_hello(instance_id(root.path()));
    let host = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        greet_client(&stream, &hello, 0);
        let mut submitted = 0;
        while let Some((rid, request)) = next_call(&stream) {
            if matches!(request, Request::Listen) {
                answer(&stream, rid, Ok(Reply::Done));
                continue;
            }
            if submitted == 0 {
                assert!(matches!(
                    request,
                    Request::SubmitShellLine {
                        quiet_ms: SUBMIT_QUIET_MS,
                        timeout_ms: SUBMIT_TIMEOUT_MS,
                        ..
                    }
                ));
            } else {
                assert!(matches!(
                    request,
                    Request::SubmitLine {
                        quiet_ms: SUBMIT_QUIET_MS,
                        confirm_ms: SUBMIT_CONFIRM_MS,
                        timeout_ms: SUBMIT_TIMEOUT_MS,
                        max_nudges: SUBMIT_MAX_NUDGES,
                        ..
                    }
                ));
            }
            // Last native request loses its acknowledgement after dispatch.
            if submitted == 3 {
                break;
            }
            answer(
                &stream,
                rid,
                Ok(Reply::Flag {
                    value: submitted == 1,
                }),
            );
            submitted += 1;
        }
    });
    let client = SessionServerClient::from_admitted_transport(
        root.path(),
        handshake(root.path()).unwrap(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let submit = |route| {
        crate::mcp::remote_terminal::submit_remote_line(
            &client,
            &sid("native-input"),
            route,
            "controlled input",
        )
    };
    assert!(!submit(InputRoute::Shell).unwrap().submitted);
    assert!(submit(InputRoute::Agent).unwrap().submitted);
    assert!(matches!(
        submit(InputRoute::Agent),
        Err(InputDeliveryError::Unknown(_))
    ));
    assert!(matches!(
        submit(InputRoute::Agent),
        Err(InputDeliveryError::Unknown(_))
    ));
    host.join().unwrap();
    client.sever();
    assert!(matches!(
        submit(InputRoute::Agent),
        Err(InputDeliveryError::NotSent(_))
    ));
}

/// A Claude a person started by hand in a shell tab is found in the foreground by
/// the owning host, so typed input reaches it through the agent protocol.
#[test]
fn a_hand_started_agent_in_the_foreground_routes_input_to_the_agent_protocol() {
    use crate::commands::pty::{agent_in_foreground, InputRoute};
    let server = TestServer::start();
    let client = SessionServerState::default()
        .connect_or_launch(server.root(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    spawn(
        &server,
        &client,
        // A stand-in Claude: the process runs as `claude` (argv[0]), as a native
        // install does.
        "exec -a claude /bin/sleep 60",
        registration("hand-started-agent", None, false),
    );
    spawn(
        &server,
        &client,
        "exec /bin/sleep 60",
        registration("plain-shell", None, false),
    );
    assert!(wait_until(|| agent_in_foreground(
        client.as_ref(),
        &sid("hand-started-agent")
    )));
    assert!(!agent_in_foreground(client.as_ref(), &sid("plain-shell")));
    assert_eq!(
        InputRoute::for_tab(false, || agent_in_foreground(
            client.as_ref(),
            &sid("hand-started-agent")
        )),
        InputRoute::Agent
    );
    assert_eq!(
        InputRoute::for_tab(false, || agent_in_foreground(
            client.as_ref(),
            &sid("plain-shell")
        )),
        InputRoute::Shell
    );
    // A tab launched as an agent is one without asking its host.
    assert_eq!(
        InputRoute::for_tab(true, || panic!("not probed")),
        InputRoute::Agent
    );
    for session in ["hand-started-agent", "plain-shell"] {
        client.kill(&sid(session)).unwrap();
    }
}

/// What a mock host saw of one `execute(Run)`: the submit protocol it was asked
/// to run, and whether the foreground was probed first.
#[derive(Debug, Default, PartialEq, Eq)]
struct SeenInput {
    probed: bool,
    agent_submit: bool,
    shell_submit: bool,
}

/// Run one remote `execute(Run{target})` against a mock host whose layout shows the
/// record's tab and whose foreground probe answers `claude_in_foreground`.
fn remote_run_against_mock_host(
    launch: codesense_host_proto::agent_cli::NativeLaunch,
    target: crate::mcp::remote_terminal::InputTarget,
    claude_in_foreground: bool,
) -> (serde_json::Value, SeenInput) {
    use crate::commands::machines::{
        RuntimeCounter, RuntimeMemberAddress, RuntimeMemberTarget, RuntimeSessionTarget,
    };
    use crate::commands::pty_layout::RemoteShellRecord;
    use crate::mcp::remote_terminal::{execute, RemoteOperation};
    use codesense_pty_core::domain::ServerId;
    use codesense_pty_core::layout::{
        EntryState, LayoutSnapshot, Placement, TabEntry, ViewerLayout, WorkspaceLayout,
    };
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let mut hello = mock_hello(instance_id(root.path()));
    hello
        .capabilities
        .extend([Capability::WritableAttachment, Capability::AgentProcessV1]);
    let request_id = uuid::Uuid::new_v4();
    let workspace = crate::commands::pty_layout::workspace_key("org:team", "work");
    let session = sid("remote-input-tab");
    let snapshot = {
        let mut tags = codesense_pty_core::layout::Tags::new();
        tags.insert("remoteHandle".into(), "remote-tab-h".into());
        tags.insert("remoteRequestId".into(), request_id.to_string());
        let mut workspaces = std::collections::BTreeMap::new();
        workspaces.insert(
            workspace.clone(),
            WorkspaceLayout {
                tabs: vec![TabEntry {
                    session: session.clone(),
                    name: "t".into(),
                    role: None,
                    placement: Placement::Tab,
                    state: EntryState::Live,
                    tags,
                }],
                ..Default::default()
            },
        );
        LayoutSnapshot {
            schema: codesense_pty_core::layout::LAYOUT_SCHEMA,
            server_id: ServerId::new("mock"),
            server_name: "mock".into(),
            epoch: "e".into(),
            rev: 1,
            workspaces,
            viewer: ViewerLayout::default(),
        }
    };
    let host = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        greet_client(&stream, &hello, 0);
        let mut seen = SeenInput::default();
        while let Some((rid, request)) = next_call(&stream) {
            let reply = match request {
                Request::LayoutSnapshot { .. } => Reply::Layout {
                    snapshot: Box::new(snapshot.clone()),
                },
                Request::AgentsAlive { queries } => {
                    seen.probed = true;
                    Reply::AgentsAlive {
                        alive: queries
                            .iter()
                            .map(|(_, name)| claude_in_foreground && name == "claude")
                            .collect(),
                    }
                }
                Request::SubmitLine { .. } => {
                    seen.agent_submit = true;
                    Reply::Flag { value: true }
                }
                Request::SubmitShellLine { .. } => {
                    seen.shell_submit = true;
                    Reply::Flag { value: true }
                }
                _ => Reply::Done,
            };
            answer(&stream, rid, Ok(reply));
            if seen.agent_submit || seen.shell_submit {
                break;
            }
        }
        seen
    });
    let client = SessionServerClient::from_admitted_transport(
        root.path(),
        handshake(root.path()).unwrap(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let record = RemoteShellRecord {
        workspace_story_id: None,
        agent: None,
        phase: Default::default(),
        handle: "remote-tab-h".into(),
        owner_id: 41,
        request_id,
        machine_id: "remote".into(),
        team_key: "org:team".into(),
        workspace_id: "work".into(),
        workspace,
        cwd: "/tmp".into(),
        name: None,
        mcp_profile_revision: None,
        mcp_inventory: Vec::new(),
        hooks: false,
        launch,
        permission_mode: Default::default(),
        status: "opened",
        identity: Default::default(),
        target: RuntimeSessionTarget {
            runtime: RuntimeMemberTarget {
                member: RuntimeMemberAddress::remote("remote"),
                admission_revision: RuntimeCounter::new(1),
                namespace: client.namespace().into(),
            },
            session,
        },
    };
    let result = execute(
        &client,
        &record,
        RemoteOperation::Run {
            command: "fix the build".into(),
            target,
        },
        || Ok(()),
    );
    let seen = host.join().unwrap();
    client.sever();
    (result, seen)
}

/// The founder's case: a shell-launched remote tab running a hand-started Claude.
/// The host's foreground probe sends the prompt through the agent protocol.
#[test]
fn remote_run_routes_a_shell_tab_running_claude_to_the_agent_protocol() {
    use crate::mcp::remote_terminal::InputTarget;
    use codesense_host_proto::agent_cli::NativeLaunch;
    let (result, seen) = remote_run_against_mock_host(NativeLaunch::Shell, InputTarget::Auto, true);
    assert_eq!(result["status"], "submitted", "{result}");
    assert_eq!(result["input"], "agent");
    assert_eq!(
        seen,
        SeenInput {
            probed: true,
            agent_submit: true,
            shell_submit: false
        }
    );
}

/// A shell tab with no agent in the foreground keeps the shell protocol.
#[test]
fn remote_run_keeps_the_shell_protocol_for_a_shell_prompt() {
    use crate::mcp::remote_terminal::InputTarget;
    use codesense_host_proto::agent_cli::NativeLaunch;
    let (result, seen) =
        remote_run_against_mock_host(NativeLaunch::Shell, InputTarget::Auto, false);
    assert_eq!(result["status"], "submitted", "{result}");
    assert_eq!(result["input"], "shell");
    assert_eq!(
        seen,
        SeenInput {
            probed: true,
            agent_submit: false,
            shell_submit: true
        }
    );
}

/// A tab launched as an agent, and any `send_to_session` (target Agent), use the
/// agent protocol without asking the host what runs there.
#[test]
fn remote_run_uses_the_agent_protocol_for_agent_launches_and_send_to_session() {
    use crate::mcp::remote_terminal::InputTarget;
    use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
    for (launch, target) in [
        (NativeLaunch::Cli(AgentCli::Claude), InputTarget::Auto),
        (NativeLaunch::Cli(AgentCli::Codex), InputTarget::Auto),
        (NativeLaunch::Shell, InputTarget::Agent),
    ] {
        let (result, seen) = remote_run_against_mock_host(launch, target, false);
        assert_eq!(result["input"], "agent", "{launch:?} {target:?}: {result}");
        assert_eq!(
            seen,
            SeenInput {
                probed: false,
                agent_submit: true,
                shell_submit: false
            },
            "{launch:?} {target:?}"
        );
    }
}

/// CX3: both callers of send_to_session use the owning host's startup and
/// turn-proof contract. A redraw after Enter is insufficient; qualified hooks
/// must confirm the first turn. This uses real PTYs and the real wire protocol.
#[test]
fn cx3_send_to_a_starting_codex_local_and_remote_requires_turn_start() {
    use crate::commands::pty::InputRoute;
    use codesense_pty_core::activity::{Event, Report, Scope};
    use codesense_pty_core::agent_cli::AgentCli;
    for remote in [false, true] {
        for confirm in [false, true] {
            let server = TestServer::start();
            server.host().start_activity(server.root()).unwrap();
            let client = SessionServerState::default()
                .connect_or_launch(server.root(), Arc::new(RecordingSink::default()), no_launch)
                .unwrap();
            let owner = server.host().terminal().activity().clone();
            let writer = owner
                .prepare_native("cx3-starting", AgentCli::Codex, "unused")
                .unwrap();
            let pid = spawn(&server, &client,
                "printf '\\033[?2004h> '; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; for n in {1..15}; do printf '.'; sleep 0.1; done; done",
                registration("cx3-starting", None, false));
            assert!(wait_until(|| server.text("cx3-starting").contains('>')));
            assert!(!owner.snapshot("cx3-starting").unwrap().observed);
            let session = sid("cx3-starting");
            client.claim_writable_typed(&session).unwrap();
            std::thread::scope(|scope| {
                let sending = scope.spawn(|| {
                    if remote {
                        let result = crate::mcp::remote_terminal::submit_remote_line(
                            &client,
                            &session,
                            InputRoute::Agent,
                            "first prompt",
                        );
                        if confirm {
                            result.unwrap().submitted
                        } else {
                            assert!(
                                matches!(result, Err(InputDeliveryError::Unknown(_))),
                                "{result:?}"
                            );
                            false
                        }
                    } else {
                        client
                            .submit_line(&session, "first prompt", 200, 400, 4000, 3)
                            .unwrap()
                            .submitted
                    }
                });
                std::thread::sleep(Duration::from_millis(150));
                assert!(
                    !server.text("cx3-starting").contains("first prompt"),
                    "startup is still unobserved"
                );
                let report = |n, event| {
                    owner
                        .report_for_test(
                            &writer,
                            pid,
                            Report {
                                provider: AgentCli::Codex,
                                event,
                                scope: Scope::Main,
                                report_id: format!("r{n}"),
                                provider_session: Some("thread-1".into()),
                                turn: None,
                                tool: None,
                                tool_name: None,
                                reason: None,
                                agent: None,
                                subagents: None,
                            },
                        )
                        .unwrap();
                };
                report(0, Event::Started);
                assert!(wait_until(|| server.text("cx3-starting").contains("got:")));
                if confirm {
                    report(1, Event::Submitted);
                }
                assert_eq!(
                    sending.join().unwrap(),
                    confirm,
                    "remote={remote}, confirm={confirm}"
                );
            });
            assert_eq!(
                server.text("cx3-starting").matches("first prompt").count(),
                2,
                "one echo and one processed prompt; Enter nudges may add empty lines"
            );
            client.kill(&session).unwrap();
        }
    }
}
