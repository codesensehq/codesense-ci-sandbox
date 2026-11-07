// SPDX-License-Identifier: MIT
#[path = "manifest_wire_tests.rs"]
mod manifest_wire_tests;
#[path = "workspace_upload_tests.rs"]
mod workspace_upload_tests;
include!("mcp_input_delivery_tests.rs");
#[path = "hook_trust_tests.rs"]
mod hook_trust_tests;
use super::*;

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixListener;
use std::sync::atomic::AtomicUsize;
use std::sync::Condvar;

use codesense_host_proto::{read_hello, Census, Hello, PeerHello, PROTO_MAJOR};
use codesense_pty_core::domain::{
    PtySessionId, TeardownReason, TerminalDomain, TerminalDomainEvent, WorkspaceKey,
};
use codesense_pty_core::layout::ServerIdentity;
use codesense_pty_core::osc::OscEvent;
use codesense_pty_core::protocol::Egress;
use codesense_pty_core::runner::{ExitInfo, LocalPtyRunner, PtyRunner};
use codesense_pty_core::spawn::SpawnIntent;
use codesense_pty_core::term_state::StreamId;
use codesense_pty_core::TerminalHost;
use codesense_session_server::testing::{
    alive, registration, sh, sh_spec, short_root, sidecar_pids_rooted_at, tie_to_this_process,
    wait_until, ReapSidecarsAt, RecordingSink, TestServer,
};

#[test]
fn machine_stats_reads_cache_for_thirty_seconds_and_old_servers_stay_unknown() {
    for supported in [false, true] {
        let root = short_root();
        let listener = UnixListener::bind(socket_path(root.path())).unwrap();
        let mut hello = mock_hello(instance_id(root.path()));
        if supported {
            hello.capabilities.push(Capability::MachineStatsV1);
        }
        let reads = Arc::new(AtomicUsize::new(0));
        let seen = reads.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            greet_client(&stream, &hello, 0);
            while let Some((rid, request)) = next_call(&stream) {
                match request {
                    Request::Listen => answer(&stream, rid, Ok(Reply::Done)),
                    Request::MachineStats | Request::MachineStatsWatch { watch: true } => {
                        seen.fetch_add(1, Ordering::SeqCst);
                        let sample = codesense_host_proto::machine_stats::MachineStats {
                            load1: 3.04,
                            cpu_count: 8,
                            mem_used_bytes: 12,
                            mem_total_bytes: 16,
                            disk_free_bytes: 84,
                            disk_total_bytes: 460,
                            disk_path: "/workspaces".into(),
                            sampled_at: chrono::Utc::now().timestamp_millis() as u64,
                        };
                        answer(
                            &stream,
                            rid,
                            Ok(Reply::MachineStats {
                                sample: Some(sample),
                            }),
                        );
                    }
                    Request::MachineStatsWatch { watch: false } => {
                        answer(&stream, rid, Ok(Reply::MachineStats { sample: None }))
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
        });
        let client = SessionServerClient::connect(
            root.path(),
            Arc::new(RecordingSink::default()),
            Arc::default(),
            None,
        )
        .unwrap();
        assert_eq!(client.read_machine_stats().unwrap().is_some(), supported);
        assert_eq!(client.read_machine_stats().unwrap().is_some(), supported);
        assert_eq!(reads.load(Ordering::SeqCst), usize::from(supported));
        if supported {
            lock(&client.machine_stats).as_mut().unwrap().0 =
                Instant::now() - Duration::from_secs(31);
            client.read_machine_stats().unwrap();
            assert_eq!(reads.load(Ordering::SeqCst), 2);
        }
        client.set_machine_stats_watching(true);
        assert_eq!(
            client.sync_machine_stats_watch().unwrap().is_some(),
            supported
        );
        client.set_machine_stats_watching(false);
        assert!(client.sync_machine_stats_watch().unwrap().is_none());
        drop(client);
        server.join().unwrap();
    }
}

fn spawn(
    sidecar: &TestServer,
    client: &SessionServerClient,
    script: &str,
    reg: SessionRegistration,
) -> u32 {
    sidecar.track(&reg.session_id);
    client
        .spawn(sh(script), reg)
        .expect("sidecar spawn")
        .expect("child pid")
}

fn sid(id: &str) -> PtySessionId {
    PtySessionId::new(id)
}

pub(crate) fn no_launch(_: &Path) -> Result<(), String> {
    Err("no launch expected".into())
}

struct DropOneSpawnReply {
    inner: BufReader<Box<dyn TransportReader>>,
    ready: VecDeque<u8>,
    dropped: Arc<AtomicBool>,
}
impl TransportReader for DropOneSpawnReply {}
impl Read for DropOneSpawnReply {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        while self.ready.is_empty() {
            let Some(frame) = read_frame(&mut self.inner).map_err(std::io::Error::other)? else {
                return Ok(0);
            };
            if matches!(
                frame.decode_json::<Message>(),
                Ok(Message::Reply {
                    result: Ok(Reply::Spawned { .. }),
                    ..
                })
            ) && !self.dropped.swap(true, Ordering::SeqCst)
            {
                continue;
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
fn remote_unknown_spawn_keeps_live_process_and_commits_reserved_correlation() {
    use codesense_pty_core::layout::{Placement, WorkspaceDescriptor};
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let unrelated_local_root = short_root();
    let admitted = handshake(root.path()).unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let transport = SessionServerTransport {
        reader: BufReader::new(Box::new(DropOneSpawnReply {
            inner: admitted.transport.reader,
            ready: VecDeque::new(),
            dropped: dropped.clone(),
        })),
        ..admitted.transport
    };
    // The transport is real; the desktop fallback socket is intentionally not
    // this remote host, so cancellation cannot pretend to confirm its death.
    let client = SessionServerClient::from_admitted_transport(
        unrelated_local_root.path(),
        AdmittedSessionTransport {
            transport,
            ..admitted
        },
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    sidecar.track("uncertain");
    let workspace = WorkspaceKey::new("org:team|work");
    let session = sid("uncertain");
    let request = OpenTab {
        viewer: ViewerId::new("app"),
        workspace: workspace.clone(),
        session: session.clone(),
        descriptor: Some(WorkspaceDescriptor {
            display_name: "work".into(),
            kind: "root".into(),
            ..Default::default()
        }),
        name: None,
        role: None,
        tags: [("remoteRequestId".into(), "original-request".into())].into(),
        placement: Placement::Tab,
        index: None,
        activate: false,
    };
    let result = client.open_remote_tab(request, || {
        client
            .spawn_with_timeout(
                sh("cat"),
                registration("uncertain", None, false),
                Duration::from_secs(2),
            )
            .map(|_| ())
    });
    assert!(
        matches!(result, Err(LayoutError::SpawnUnknown(_))),
        "{result:?}"
    );
    assert!(dropped.load(Ordering::SeqCst));
    assert!(
        client.is_alive(),
        "later finish/reconcile transport remains usable"
    );
    assert_eq!(client.has_session(&session), Ok(true));
    let snapshot = client
        .layout_snapshot(&ViewerId::new("app"), &ClientBootId::new("same-app"))
        .unwrap();
    let tabs = &snapshot.workspaces[&workspace].tabs;
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].session, session);
    assert_eq!(
        tabs[0].tags.get("remoteRequestId").map(String::as_str),
        Some("original-request")
    );
    assert!(matches!(
        client.reconcile_remote_tab(&sid("unrelated")),
        Err(LayoutError::UnknownSession(_))
    ));
    client.kill(&session).unwrap();
}

#[test]
fn remote_consumed_request_restart_preserves_one_real_process() {
    use crate::commands::dashboard_launch::{DashboardLaunchState, RemoteShellReservation};
    use crate::commands::machines::{
        RuntimeCounter, RuntimeMemberAddress, RuntimeMemberTarget, RuntimeSessionTarget,
    };
    use crate::commands::pty_layout::RemoteShellRecord;
    let root = short_root();
    let receipts = tempfile::tempdir().unwrap();
    let server = TestServer::start_at(root.path());
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let session = sid("durable-request");
    let record = RemoteShellRecord {
        workspace_story_id: None,
        agent: None,
        phase: Default::default(),
        handle: "remote-tab-durable".into(),
        owner_id: 41,
        request_id: uuid::Uuid::new_v4(),
        machine_id: "fixture".into(),
        team_key: "org:team".into(),
        workspace_id: "work".into(),
        workspace: WorkspaceKey::new("org:team|work"),
        cwd: "/tmp".into(),
        name: None,
        mcp_profile_revision: None,
        mcp_inventory: Vec::new(),
        hooks: false,
        launch: Default::default(),
        permission_mode: Default::default(),
        target: RuntimeSessionTarget {
            runtime: RuntimeMemberTarget {
                member: RuntimeMemberAddress::remote("fixture"),
                admission_revision: RuntimeCounter::new(1),
                namespace: client.namespace().into(),
            },
            session: session.clone(),
        },
        status: "pending",
        identity: Default::default(),
    };
    let dispatch = |state: &DashboardLaunchState| match state
        .reserve_remote_shell_durable(receipts.path(), record.clone())
        .unwrap()
    {
        RemoteShellReservation::Fresh => {
            spawn(
                &server,
                &client,
                "cat",
                registration(session.as_str(), None, false),
            );
        }
        RemoteShellReservation::Existing(existing) => {
            assert_eq!(existing.target.session, session);
            assert_eq!(existing.status, "unknown");
        }
    };
    let first = DashboardLaunchState::new(1);
    dispatch(&first);
    let before = client
        .live_child_pids(std::slice::from_ref(&session))
        .unwrap();
    assert_eq!(before.len(), 1);
    drop(first);
    dispatch(&DashboardLaunchState::new(1));
    assert_eq!(
        client
            .live_child_pids(std::slice::from_ref(&session))
            .unwrap(),
        before
    );
    assert_eq!(
        client.live_session_ids().unwrap(),
        [session.clone()].into_iter().collect()
    );
    client.kill(&session).unwrap();
}

#[test]
fn writable_attachment_client_preserves_readers_and_refuses_competing_input() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let owner = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let reader = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let session = sid("writable-owner");
    spawn(
        &sidecar,
        &owner,
        "cat",
        registration(session.as_str(), None, true),
    );
    owner.claim_writable(&session).unwrap();
    owner.claim_writable(&session).unwrap();
    assert_eq!(
        reader.claim_writable_typed(&session),
        Err(WritableAttachmentFailure::AttachedElsewhere)
    );
    let subscription = reader
        .subscribe(&session, &ViewportId::new("read-only"))
        .unwrap();
    reader.unsubscribe(subscription.stream_id).unwrap();
    assert!(reader.write(&session, b"forbidden").is_err());
    assert!(reader
        .submit_shell_line(&session, "forbidden", 0, 1)
        .is_err());
    owner.write(&session, b"allowed\n").unwrap();
    owner.release_writable(&session).unwrap();
    assert!(reader.write(&session, b"unclaimed").is_err());
    reader.claim_writable(&session).unwrap();
    assert!(owner.release_writable(&session).is_err());
    reader.write(&session, b"new owner\n").unwrap();
    assert_eq!(reader.has_session(&session), Ok(true));
    reader.kill(&session).unwrap();
}

#[test]
fn rm5aa_local_candidate_cleanup_preserves_installed_validation_record() {
    let mut core = LocalAdmissionCore::default();
    for (generation, phase) in [
        LocalCandidatePhase::Pending,
        LocalCandidatePhase::Lost,
        LocalCandidatePhase::Retired,
        LocalCandidatePhase::Installed,
    ]
    .into_iter()
    .enumerate()
    {
        core.candidates.insert(
            EventNamespace::for_test("local", "epoch", generation as u64),
            LocalCandidate {
                phase,
                client: Weak::new(),
            },
        );
    }
    // An abandoned construction (including reader startup failure) has no
    // strong client owner. Installed authority still requires its loss path.
    core.prune_terminal_candidates();
    assert_eq!(core.candidates.len(), 1);
    assert!(core
        .candidates
        .contains_key(&EventNamespace::for_test("local", "epoch", 3)));
}

#[test]
fn rm5aa_local_candidate_history_is_bounded_across_reconnects() {
    let root = short_root();
    let _sidecar = TestServer::start_at(root.path());
    let state = SessionServerState::default();
    let first = state
        .connect(root.path(), Arc::new(RecordingSink::default()))
        .unwrap();
    state.adopt(first.clone()).unwrap();

    for _ in 0..16 {
        let current = state
            .connect(root.path(), Arc::new(RecordingSink::default()))
            .unwrap();
        state.adopt(current.clone()).unwrap();
        let target = state
            .admission
            .target_for_namespace(RuntimeMemberSlot::Sidecar, current.namespace())
            .unwrap();
        assert!(state.admission.validate(&target));
        assert_eq!(lock(&state.admission.core).candidates.len(), 1);
        let installed = state.admission.baseline();
        // Keep the old client alive while delivering its delayed callback.
        state.admission.lost(first.namespace());
        assert!(state.adopt(first.clone()).is_err());
        assert_eq!(state.admission.baseline(), installed);
        assert!(state.admission.validate(&target));
        assert!(Arc::ptr_eq(&state.connected().unwrap(), &current));

        current.sever();
        assert!(wait_until(|| lock(&state.admission.core)
            .candidates
            .is_empty()));
        assert!(!state.admission.validate(&target));
        assert!(state.adopt(current).is_err());
    }
    first.sever();
}

#[test]
fn rm5aa_local_candidate_cleanup_preserves_live_pending_until_install() {
    let root = short_root();
    let _sidecar = TestServer::start_at(root.path());
    let state = SessionServerState::default();
    let pending = state
        .connect(root.path(), Arc::new(RecordingSink::default()))
        .unwrap();
    for _ in 0..8 {
        let lost = state
            .connect(root.path(), Arc::new(RecordingSink::default()))
            .unwrap();
        lost.sever();
        assert!(wait_until(
            || lock(&state.admission.core).candidates.len() == 1
        ));
        assert!(state.adopt(lost).is_err());
        assert!(state.admission.baseline().rows.is_empty());
    }
    state.adopt(pending.clone()).unwrap();
    assert!(Arc::ptr_eq(&state.connected().unwrap(), &pending));
    state.admission.revoke_sidecar();
    assert!(lock(&state.admission.core).candidates.is_empty());
    assert!(state.adopt(pending.clone()).is_err());
    let revoked = state.admission.baseline();
    state.admission.lost(pending.namespace());
    assert_eq!(state.admission.baseline(), revoked);

    let closing = state
        .connect(root.path(), Arc::new(RecordingSink::default()))
        .unwrap();
    state.admission.shutdown();
    assert!(lock(&state.admission.core).candidates.is_empty());
    assert!(state.adopt(closing).is_err());
    pending.sever();
}

#[test]
fn rm5aa_local_loss_before_install_never_publishes_live() {
    let root = short_root();
    let _sidecar = TestServer::start_at(root.path());
    let state = SessionServerState::default();
    let rows = Arc::new(Mutex::new(Vec::new()));
    let observed = rows.clone();
    state
        .admission
        .subscribe(Arc::new(move |row| lock(&observed).push(row)));
    let client = state
        .connect(root.path(), Arc::new(RecordingSink::default()))
        .unwrap();
    client.sever();
    assert!(wait_until(|| !client.is_alive()));
    assert!(state.adopt(client).is_err());
    assert!(state.connected().is_none());
    assert!(state.admission.baseline().rows.is_empty());
    assert!(lock(&rows).is_empty());
}

#[test]
fn rm5aa_old_local_loss_preserves_replacement_admission() {
    let root = short_root();
    let _sidecar = TestServer::start_at(root.path());
    let state = SessionServerState::default();
    let rows = Arc::new(Mutex::new(Vec::new()));
    let observed = rows.clone();
    state
        .admission
        .subscribe(Arc::new(move |row| lock(&observed).push(row)));
    let first = state
        .connect(root.path(), Arc::new(RecordingSink::default()))
        .unwrap();
    state.adopt(first.clone()).unwrap();
    let second = state
        .connect(root.path(), Arc::new(RecordingSink::default()))
        .unwrap();
    state.adopt(second.clone()).unwrap();
    let installed = state.admission.baseline();
    first.sever();
    assert!(wait_until(|| !first.is_alive()));
    assert_eq!(state.admission.baseline(), installed);
    assert_eq!(lock(&rows).len(), 2);
    assert!(Arc::ptr_eq(&state.connected().unwrap(), &second));
    let rows = lock(&rows);
    assert!(rows[0].revision < rows[1].revision);
}

#[test]
fn rm5aa_local_restart_revokes_before_termination() {
    let root = short_root();
    let _sidecar = TestServer::start_at(root.path());
    let state = SessionServerState::default();
    let client = state
        .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    let before = state.admission.baseline().rows.remove(0);
    let replacement_root = short_root();
    let older = Hello {
        proto_major: PROTO_MAJOR - 2,
        pid: 4242,
        ..mock_hello(instance_id(replacement_root.path()))
    };
    let mock = mock_host(replacement_root.path(), older, 1);
    let terminated = std::cell::Cell::new(false);
    let failure = state
        .restart(
            replacement_root.path(),
            Arc::new(RecordingSink::default()),
            |_| {
                terminated.set(true);
                assert!(state.connected().is_none());
                let revoked = state.admission.baseline().rows.remove(0);
                assert!(revoked.revision > before.revision);
                assert_eq!(revoked.state, RuntimeAdmissionState::Unavailable);
                Err("termination deliberately refused".into())
            },
            |_| panic!("must not launch after failed termination"),
        )
        .err()
        .expect("failed termination");
    assert!(mock.join().unwrap().is_err());
    assert!(terminated.get());
    assert!(!failure.old_stopped);
    // Revocation survives a failed termination while the predecessor is alive.
    assert!(client.is_alive());
    assert!(state.connected().is_none());
    let after = state.admission.baseline().rows.remove(0);
    assert!(after.revision > before.revision);
    assert_eq!(after.state, RuntimeAdmissionState::Unavailable);
    assert!(state.adopt(client).is_err());
}

#[derive(Default)]
struct MemoryBytes {
    bytes: Mutex<VecDeque<u8>>,
    ready: Condvar,
    closed: AtomicBool,
}

struct MemoryReader(Arc<MemoryBytes>);

impl Read for MemoryReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let mut bytes = lock(&self.0.bytes);
        while bytes.is_empty() && !self.0.closed.load(Ordering::SeqCst) {
            bytes = self
                .0
                .ready
                .wait(bytes)
                .unwrap_or_else(|error| error.into_inner());
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        let count = output.len().min(bytes.len());
        for slot in &mut output[..count] {
            *slot = bytes.pop_front().expect("count is bounded by queued bytes");
        }
        Ok(count)
    }
}

impl TransportReader for MemoryReader {}

struct MemoryWriter(Arc<MemoryBytes>);

impl Write for MemoryWriter {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        if self.0.closed.load(Ordering::SeqCst) {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        lock(&self.0.bytes).extend(input.iter().copied());
        self.0.ready.notify_all();
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl TransportWriter for MemoryWriter {
    fn close(&mut self) -> std::io::Result<()> {
        self.0.closed.store(true, Ordering::SeqCst);
        self.0.ready.notify_all();
        Ok(())
    }
}

impl Drop for MemoryWriter {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn memory_transport_pair() -> ((MemoryReader, MemoryWriter), (MemoryReader, MemoryWriter)) {
    let client_to_host = Arc::new(MemoryBytes::default());
    let host_to_client = Arc::new(MemoryBytes::default());
    (
        (
            MemoryReader(host_to_client.clone()),
            MemoryWriter(client_to_host.clone()),
        ),
        (MemoryReader(client_to_host), MemoryWriter(host_to_client)),
    )
}

struct BlockingWriter {
    entered: mpsc::Sender<()>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl Write for BlockingWriter {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let _ = self.entered.send(());
        let (released, changed) = &*self.release;
        let mut released = lock(released);
        while !*released {
            released = changed
                .wait(released)
                .unwrap_or_else(|error| error.into_inner());
        }
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl TransportWriter for BlockingWriter {
    fn close(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn release_writer(release: &Arc<(Mutex<bool>, Condvar)>) {
    let (released, changed) = &**release;
    *lock(released) = true;
    changed.notify_all();
}

#[test]
fn private_writer_refuses_new_enqueue_after_retirement_and_settles_committed_write() {
    let (lease, revoke) = crate::commands::auth::private_authority::test_lease(41);
    let (entered_tx, entered_rx) = mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let mailbox = WriteMailbox::start(
        Box::new(BlockingWriter {
            entered: entered_tx,
            release: release.clone(),
        }),
        Arc::new(|| {}),
        Arc::new(AtomicBool::new(true)),
    )
    .unwrap();
    mailbox.private_lease.set(lease).unwrap();
    mailbox.try_send(&Frame::input(1, 1, b"committed")).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    revoke();
    assert_eq!(
        mailbox.try_send(&Frame::input(1, 2, b"stale")),
        Err(WriteEnqueueError::Closed)
    );
    release_writer(&release);
    mailbox.close();
    assert!(mailbox.completion().wait_timeout(Duration::from_secs(1)));
    assert_eq!(mailbox.budget.used.load(Ordering::Acquire), 0);
}

#[test]
fn write_mailbox_reports_byte_and_message_backpressure_without_blocking() {
    let alive = Arc::new(AtomicBool::new(true));
    let (entered_tx, entered_rx) = mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let mailbox = WriteMailbox::start(
        Box::new(BlockingWriter {
            entered: entered_tx,
            release: release.clone(),
        }),
        Arc::new(|| {}),
        alive,
    )
    .unwrap();

    // Each frame is legal on the wire; together they exceed the byte budget.
    let half_budget = Frame::input(1, 1, &vec![0; WRITE_QUEUE_BYTES / 2]);
    mailbox.try_send(&half_budget).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        mailbox.try_send(&half_budget),
        Err(WriteEnqueueError::Backpressure(
            WriteBackpressure::Bytes { .. }
        ))
    ));

    let small = Frame::input(1, 2, b"x");
    for _ in 0..WRITE_QUEUE_MESSAGES {
        mailbox.try_send(&small).unwrap();
    }
    assert_eq!(
        mailbox.try_send(&small),
        Err(WriteEnqueueError::Backpressure(
            WriteBackpressure::Messages {
                capacity: WRITE_QUEUE_MESSAGES
            }
        ))
    );
    mailbox.close();
    release_writer(&release);
    assert!(mailbox.completion().wait_timeout(Duration::from_secs(1)));
}

#[test]
fn a_blocked_writer_cannot_delay_close_or_drop() {
    let alive = Arc::new(AtomicBool::new(true));
    let (entered_tx, entered_rx) = mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let release_on_interrupt = release.clone();
    let mailbox = WriteMailbox::start(
        Box::new(BlockingWriter {
            entered: entered_tx,
            release: release.clone(),
        }),
        Arc::new(move || release_writer(&release_on_interrupt)),
        alive,
    )
    .unwrap();
    let completion = mailbox.completion();
    mailbox.try_send(&Frame::input(1, 1, b"blocked")).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();

    let started = Instant::now();
    mailbox.close();
    drop(mailbox);
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "close/drop waited on the blocked writer"
    );
    assert!(completion.wait_timeout(Duration::from_secs(1)));
}

#[test]
fn a_worker_failure_interrupts_the_transport_once() {
    struct FailedWriter;
    impl Write for FailedWriter {
        fn write(&mut self, _input: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl TransportWriter for FailedWriter {
        fn close(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let interrupts = Arc::new(AtomicUsize::new(0));
    let counted = interrupts.clone();
    let alive = Arc::new(AtomicBool::new(true));
    let mailbox = WriteMailbox::start(
        Box::new(FailedWriter),
        Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
        alive.clone(),
    )
    .unwrap();
    mailbox.try_send(&Frame::input(1, 1, b"fail")).unwrap();
    assert!(mailbox.completion().wait_timeout(Duration::from_secs(1)));
    assert!(!alive.load(Ordering::SeqCst));
    assert_eq!(interrupts.load(Ordering::SeqCst), 1);
}

#[test]
fn a_writer_spawn_failure_drops_the_writer_without_a_supervisor_or_sender_leak() {
    struct DropWriter(Arc<AtomicUsize>);
    impl Write for DropWriter {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            Ok(input.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl TransportWriter for DropWriter {
        fn close(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Drop for DropWriter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let drops = Arc::new(AtomicUsize::new(0));
    let error = WriteMailbox::start_with(
        Box::new(DropWriter(drops.clone())),
        Arc::new(|| {}),
        Arc::new(AtomicBool::new(true)),
        |run| {
            drop(run);
            Err(std::io::Error::other("injected writer spawn failure"))
        },
    )
    .err()
    .expect("spawn failure is returned");
    assert_eq!(error.to_string(), "injected writer spawn failure");
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

/// The one session-server client consumes only framed byte halves: neither
/// its request/reply loop nor its `TerminalDomain` surface depends on UDS.
#[test]
fn one_client_runs_over_an_in_memory_byte_transport() {
    let root = short_root();
    let ((client_reader, client_writer), (mut host_reader, mut host_writer)) =
        memory_transport_pair();
    let host = std::thread::spawn(move || {
        let mut calls = Vec::new();
        while let Some(frame) = read_frame(&mut host_reader).unwrap() {
            let Message::Call { rid, request } = frame.decode_json().unwrap() else {
                panic!("expected a client call");
            };
            let result = match &request {
                Request::Listen => Ok(Reply::Done),
                Request::HasSession { .. } => Ok(Reply::Flag { value: true }),
                other => panic!("unexpected request {other:?}"),
            };
            calls.push(request);
            write_frame(
                &mut host_writer,
                &Frame::control(&Message::Reply { rid, result }).unwrap(),
            )
            .unwrap();
            if calls.len() == 2 {
                break;
            }
        }
        calls
    });

    let hello = mock_hello(instance_id(root.path()));
    let interrupted = client_writer.0.clone();
    let client = SessionServerClient::from_admitted_transport(
        root.path(),
        AdmittedSessionTransport {
            transport: SessionServerTransport::new(client_reader, client_writer, move || {
                interrupted.closed.store(true, Ordering::SeqCst);
                interrupted.ready.notify_all();
            }),
            hello,
            remote_close: None,
        },
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .expect("in-memory client");
    assert_eq!(client.has_session(&sid("remote")), Ok(true));
    drop(client);

    assert_eq!(
        host.join().unwrap(),
        vec![
            Request::Listen,
            Request::HasSession {
                session: sid("remote")
            }
        ]
    );
}

#[test]
fn remote_client_transition_accepts_only_the_opaque_admitted_value() {
    fn accepts_opaque<F, Fut>(_constructor: F)
    where
        F: Fn(
            AdmittedRemoteTransport,
            Arc<dyn NamespacedTerminalEventSink>,
            Arc<Ledger>,
            Option<LostHook>,
        ) -> Fut,
        Fut: std::future::Future<Output = Result<ManagedRemoteClient, ConnectError>>,
    {
    }
    accepts_opaque(SessionServerClient::from_remote_admitted);
}

#[test]
fn remote_publication_gate_suppresses_pre_adoption_events_and_lost_callbacks() {
    let sink = Arc::new(RecordingSink::default());
    let lost = Arc::new(AtomicUsize::new(0));
    let lost_hook: LostHook = Arc::new(Mutex::new(Some({
        let lost = lost.clone();
        Arc::new(move || {
            lost.fetch_add(1, Ordering::SeqCst);
        }) as OnLost
    })));
    let gate = RemotePublicationGate::new(sink.clone(), Some(lost_hook));
    RemotePublicationGate::emit(
        &gate,
        TerminalDomainEvent::LayoutChanged {
            epoch: "pre-adoption".into(),
            rev: 1,
            workspace: WorkspaceKey::new("ws"),
            layout: None,
        },
    )
    .unwrap();
    gate.lost();
    assert!(sink.events().is_empty());
    assert_eq!(lost.load(Ordering::SeqCst), 0);
    gate.suppress();
    gate.publish();
    assert!(sink.events().is_empty());
    assert_eq!(lost.load(Ordering::SeqCst), 0);
}

#[test]
fn published_loss_hook_may_reenter_hook_configuration_without_deadlock() {
    let gate = RemotePublicationGate::new(Arc::new(RecordingSink::default()), None);
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_gate = gate.clone();
    let callback_calls = calls.clone();
    *lock(&gate.lost) = Some(Arc::new(Mutex::new(Some(Arc::new(move || {
        callback_calls.fetch_add(1, Ordering::SeqCst);
        *lock(&callback_gate.lost) = None;
    }) as OnLost))));
    gate.publish();
    gate.lost();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(lock(&gate.lost).is_none());
}

#[test]
fn lost_before_hook_install_is_replayed_and_prepared_delivery_revalidates() {
    let sink = Arc::new(RecordingSink::default());
    let gate = RemotePublicationGate::new(sink.clone(), None);
    gate.lost();
    let lost = Arc::new(AtomicUsize::new(0));
    let hook: LostHook = Arc::new(Mutex::new(Some({
        let lost = lost.clone();
        Arc::new(move || {
            lost.fetch_add(1, Ordering::SeqCst);
        }) as OnLost
    })));
    *lock(&gate.lost) = Some(hook);
    assert!(gate.is_lost());
    assert!(gate.prepare().is_none());
    assert_eq!(lost.load(Ordering::SeqCst), 0);

    let gate = RemotePublicationGate::new(sink.clone(), None);
    let prepared = gate.prepare().expect("candidate prepares");
    RemotePublicationGate::emit(
        &gate,
        TerminalDomainEvent::LayoutChanged {
            epoch: "between-prepare-and-delivery".into(),
            rev: 1,
            workspace: WorkspaceKey::new("ws"),
            layout: None,
        },
    )
    .unwrap();
    assert!(!prepared.deliver_if(|| false, || true));
    assert!(sink.events().is_empty());
}

#[test]
fn remote_loss_gate_is_isolated_from_the_local_sidecar_hook() {
    let state = SessionServerState::default();
    let local_lost = Arc::new(AtomicUsize::new(0));
    state.on_lost({
        let local_lost = local_lost.clone();
        Arc::new(move || {
            local_lost.fetch_add(1, Ordering::SeqCst);
        })
    });
    let remote_lost = Arc::new(AtomicUsize::new(0));
    let remote_hook: LostHook = Arc::new(Mutex::new(Some({
        let remote_lost = remote_lost.clone();
        Arc::new(move || {
            remote_lost.fetch_add(1, Ordering::SeqCst);
        }) as OnLost
    })));
    let gate = RemotePublicationGate::new(Arc::new(RecordingSink::default()), None);
    gate.lost();
    gate.publish();
    assert_eq!(local_lost.load(Ordering::SeqCst), 0);
    assert_eq!(remote_lost.load(Ordering::SeqCst), 0);

    let published =
        RemotePublicationGate::new(Arc::new(RecordingSink::default()), Some(remote_hook));
    published.publish();
    published.lost();
    assert_eq!(remote_lost.load(Ordering::SeqCst), 1);
    assert_eq!(local_lost.load(Ordering::SeqCst), 0);
}

fn stale_socket_fixture(path: &Path) -> OwnedFd {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un contains only integer fields; zeroes also terminate sun_path.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    assert!(
        !bytes.is_empty() && !bytes.contains(&0) && bytes.len() < address.sun_path.len(),
        "invalid stale socket fixture path {path:?}: expected 1..{} non-NUL bytes",
        address.sun_path.len()
    );
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    let address_len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = address_len as u8;
    }

    let socket_type = libc::SOCK_STREAM;
    #[cfg(target_os = "linux")]
    let socket_type = socket_type | libc::SOCK_CLOEXEC;
    // SAFETY: socket takes scalar arguments and returns a new descriptor on success.
    let raw_fd = unsafe { libc::socket(libc::AF_UNIX, socket_type, 0) };
    assert!(
        raw_fd >= 0,
        "create stale socket fixture {path:?}: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: the successful socket call transferred sole ownership of this descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: fd is live, and F_SETFD takes an integer descriptor-flags argument.
        let cloexec = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        assert!(
            cloexec == 0,
            "set close-on-exec for stale socket fixture {path:?}: {}",
            std::io::Error::last_os_error()
        );
    }
    // Never listen: retaining a duplicated or inherited descriptor alone does
    // not start listening, even after the original FD closes.
    // SAFETY: address is initialized, and address_len includes the path's NUL
    // terminator within its allocation. fd remains owned through the call.
    let bound = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            address_len,
        )
    };
    assert!(
        bound == 0,
        "bind stale socket fixture {path:?}: {}",
        std::io::Error::last_os_error()
    );
    fd
}

#[test]
fn stale_socket_fixture_refuses_connections_with_a_retained_descriptor() {
    let root = short_root();
    let path = socket_path(root.path());
    let fixture = stale_socket_fixture(&path);
    // SAFETY: fixture owns a live descriptor, and F_GETFD takes no third argument.
    let flags = unsafe { libc::fcntl(fixture.as_raw_fd(), libc::F_GETFD) };
    assert!(
        flags >= 0,
        "read stale socket fixture descriptor flags: {}",
        std::io::Error::last_os_error()
    );
    assert_ne!(
        flags & libc::FD_CLOEXEC,
        0,
        "stale socket fixture descriptor must have close-on-exec set, got flags {flags}"
    );
    // A clone models a descriptor inherited by a concurrent fork before exec.
    let retained = fixture.try_clone().expect("clone stale socket fixture FD");
    drop(fixture);
    assert!(
        std::fs::symlink_metadata(&path)
            .expect("stat stale socket fixture")
            .file_type()
            .is_socket(),
        "stale fixture must leave a socket at {path:?}"
    );

    let result = UnixStream::connect(&path);
    assert!(
        matches!(&result, Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused),
        "stale socket with a retained descriptor must refuse connections, got {result:?}"
    );
    drop(retained);
}

#[test]
fn stale_socket_fixture_reports_invalid_paths_and_bind_failures() {
    let failure = |path: &Path| {
        let panic = std::panic::catch_unwind(|| stale_socket_fixture(path))
            .expect_err("invalid fixture setup must panic");
        panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                panic
                    .downcast_ref::<&str>()
                    .map(|message| (*message).to_owned())
            })
            .expect("fixture panic must contain a diagnostic")
    };
    let overlong = "x".repeat(std::mem::size_of::<libc::sockaddr_un>());
    for path in [Path::new(""), Path::new("bad\0path"), Path::new(&overlong)] {
        let message = failure(path);
        assert!(
            message.contains(&format!("invalid stale socket fixture path {path:?}"))
                && message.contains("non-NUL bytes"),
            "invalid path diagnostic must identify the path and requirement, got {message:?}"
        );
    }

    let root = short_root();
    let path = socket_path(root.path());
    let _occupied = stale_socket_fixture(&path);
    // The failed bind drops its owned descriptor as the panic unwinds.
    let message = failure(&path);
    let expected_error = std::io::Error::from_raw_os_error(libc::EADDRINUSE);
    assert!(
        message.contains(&format!("bind stale socket fixture {path:?}"))
            && message.contains(&expected_error.to_string()),
        "bind diagnostic must identify the path and OS error, got {message:?}"
    );
}

/// Fail-open: an absent socket and a stale socket file are unreachable (a
/// launch may help); a sidecar serving another instance is refused (it never
/// will); only this instance's own sidecar connects.
#[test]
fn only_this_instances_sidecar_connects() {
    let sink = || -> Arc<dyn TerminalEventSink> { Arc::new(RecordingSink::default()) };

    let absent = short_root();
    assert!(matches!(
        SessionServerClient::connect(absent.path(), sink(), Arc::default(), None),
        Err(ConnectError::Unreachable(_))
    ));

    let stale = short_root();
    drop(stale_socket_fixture(&socket_path(stale.path())));
    assert!(
        std::fs::symlink_metadata(socket_path(stale.path()))
            .expect("stat stale socket fixture after closing its final FD")
            .file_type()
            .is_socket(),
        "stale socket file left"
    );
    match SessionServerClient::connect(stale.path(), sink(), Arc::default(), None) {
        Err(ConnectError::Unreachable(_)) => {}
        other => panic!(
            "a stale socket must be unreachable, got {:?}",
            other.map(|_| ())
        ),
    }

    let other = short_root();
    let _other_sidecar = TestServer::start_at(other.path());
    let foreign = short_root();
    std::os::unix::fs::symlink(socket_path(other.path()), socket_path(foreign.path())).unwrap();
    match SessionServerClient::connect(foreign.path(), sink(), Arc::default(), None) {
        Err(ConnectError::Foreign(message)) => {
            assert!(message.contains("is served for instance"), "{message}")
        }
        other => panic!(
            "a foreign sidecar must be refused, got {:?}",
            other.map(|_| ())
        ),
    }

    let own = SessionServerClient::connect(other.path(), sink(), Arc::default(), None)
        .expect("own sidecar");
    assert_eq!(
        own.pid(),
        std::process::id(),
        "the in-process sidecar answers"
    );
    assert!(own.is_alive());
}

/// A launch is attempted only when nothing listens, at most once per backoff
/// window, and never for a foreign sidecar; a successful launch connects, and
/// later calls share its one connection.
#[test]
fn connect_or_launch_launches_once_per_backoff_and_never_over_a_foreign_sidecar() {
    let sink = || -> Arc<dyn TerminalEventSink> { Arc::new(RecordingSink::default()) };
    let calls = std::cell::Cell::new(0);
    let failing = |_: &Path| {
        calls.set(calls.get() + 1);
        Err("no binary".to_string())
    };

    let absent = short_root();
    let state = SessionServerState::default();
    assert!(state
        .connect_or_launch(absent.path(), sink(), failing)
        .is_err());
    assert!(state
        .connect_or_launch(absent.path(), sink(), failing)
        .is_err());
    assert_eq!(calls.get(), 1, "no relaunch inside the backoff window");
    assert!(state.reconnected(absent.path()).is_none());

    let other = short_root();
    let _other_sidecar = TestServer::start_at(other.path());
    let foreign = short_root();
    std::os::unix::fs::symlink(socket_path(other.path()), socket_path(foreign.path())).unwrap();
    let state = SessionServerState::default();
    assert!(state
        .connect_or_launch(foreign.path(), sink(), failing)
        .is_err());
    assert_eq!(calls.get(), 1, "a foreign sidecar is never launched over");
    assert!(
        !state.launched_sidecar(),
        "a failed launch launched nothing"
    );

    let running = short_root();
    let _running_sidecar = TestServer::start_at(running.path());
    let state = SessionServerState::default();
    state
        .connect_or_launch(running.path(), sink(), no_launch)
        .unwrap();
    assert!(
        !state.launched_sidecar(),
        "a sidecar found running was not launched by this app"
    );

    let fresh = short_root();
    let started = Mutex::new(None);
    let state = SessionServerState::default();
    let client = state
        .connect_or_launch(fresh.path(), sink(), |root| {
            *lock(&started) = Some(TestServer::start_at(root));
            Ok(())
        })
        .expect("launched sidecar connects");
    let again = state
        .connect_or_launch(fresh.path(), sink(), no_launch)
        .unwrap();
    assert!(Arc::ptr_eq(&client, &again), "one live connection");
    assert!(state.launched_sidecar(), "this app launched the sidecar");
}

/// A non-zero child exit reaches the app-side sink with its real status, and a
/// signal death carries its signal.
#[test]
fn the_real_exit_status_crosses_the_boundary() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let events = Arc::new(RecordingSink::default());
    let client =
        SessionServerClient::connect(root.path(), events.clone(), Arc::default(), None).unwrap();

    spawn(
        &sidecar,
        &client,
        "exit 42",
        registration("exit-42", None, false),
    );
    assert!(wait_until(|| events.exit("exit-42").is_some()));
    assert_eq!(
        events.exit("exit-42"),
        Some(Some(ExitInfo {
            exit_code: Some(42),
            signal: None
        }))
    );

    spawn(
        &sidecar,
        &client,
        "kill -9 $$",
        registration("sig-9", None, false),
    );
    assert!(wait_until(|| events.exit("sig-9").is_some()));
    let status = events.exit("sig-9").flatten().expect("status collected");
    assert_eq!(status.exit_code, None);
    assert!(status.signal.is_some(), "a signal death names its signal");
}

/// The same command produces the same delivered bytes whether pty-core runs
/// in-process or in the sidecar.
#[test]
fn delivered_output_is_byte_identical_across_hosts() {
    let script = "printf 'RT6:\\303\\251\\342\\224\\200\\n'; i=0; while [ $i -lt 50 ]; do printf 'L%s\\n' $i; i=$((i+1)); done";

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let local_events = Arc::new(RecordingSink::default());
    let local = TerminalHost::new(
        ServerIdentity::ephemeral("test"),
        local_events.clone(),
        runtime.handle().clone(),
    );
    let pty = runtime
        .block_on(LocalPtyRunner.spawn(sh_spec("same", script)))
        .unwrap();
    local
        .register(pty, registration("same", None, false))
        .unwrap();
    assert!(wait_until(|| local_events.exit("same").is_some()));

    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let remote_events = Arc::new(RecordingSink::default());
    let client =
        SessionServerClient::connect(root.path(), remote_events.clone(), Arc::default(), None)
            .unwrap();
    spawn(&sidecar, &client, script, registration("same", None, false));
    assert!(wait_until(|| remote_events.exit("same").is_some()));

    let local_output = local_events.output("same");
    assert!(local_output.contains("RT6:\u{e9}\u{2500}") && local_output.contains("L49"));
    assert_eq!(remote_events.output("same"), local_output);
    local.kill(&sid("same")).unwrap();
}

/// The `pty-exit` payload the app emits for `id`, from the host's `Exit`.
fn exit_payload(
    events: &RecordingSink,
    id: &str,
) -> Option<crate::commands::pty_host::PtyExitPayload> {
    events.events().into_iter().find_map(|event| match event {
        TerminalDomainEvent::Exit {
            session,
            status,
            requested,
            ..
        } if session.as_str() == id => Some(crate::commands::pty_host::pty_exit_payload(
            id, status, requested,
        )),
        _ => None,
    })
}

/// A child's end reads the same whichever host runs it: exit 0 is clean, a
/// nonzero exit or a signal death is abnormal, and the host's own kill is a
/// requested exit.
#[test]
fn exit_classification_is_identical_across_hosts() {
    use crate::agent_state::{classify_exit, ExitClass};

    let cases = [
        ("clean", "exit 0", ExitClass::Clean),
        ("failed", "exit 5", ExitClass::Abnormal),
        ("sigkill", "kill -9 $$", ExitClass::Abnormal),
        ("stopped", "sleep 30", ExitClass::Abnormal),
    ];

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let local_events = Arc::new(RecordingSink::default());
    let local = TerminalHost::new(
        ServerIdentity::ephemeral("test"),
        local_events.clone(),
        runtime.handle().clone(),
    );
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let remote_events = Arc::new(RecordingSink::default());
    let client =
        SessionServerClient::connect(root.path(), remote_events.clone(), Arc::default(), None)
            .unwrap();
    for (id, script, _) in cases {
        let pty = runtime
            .block_on(LocalPtyRunner.spawn(sh_spec(id, script)))
            .unwrap();
        local.register(pty, registration(id, None, false)).unwrap();
        spawn(&sidecar, &client, script, registration(id, None, false));
    }
    local.kill(&sid("stopped")).unwrap();
    client.kill(&sid("stopped")).unwrap();

    for (id, _, class) in cases {
        assert!(wait_until(|| exit_payload(&local_events, id).is_some()
            && exit_payload(&remote_events, id).is_some()));
        let here = exit_payload(&local_events, id).unwrap();
        let there = exit_payload(&remote_events, id).unwrap();
        let code = here["exit_code"].as_i64();
        assert_eq!(
            classify_exit(code, here["signal"].as_str()),
            class,
            "{id}: {here}"
        );
        assert_eq!(there, here, "{id}: the sidecar's exit reads the same");
    }
    assert_eq!(
        exit_payload(&local_events, "stopped").unwrap()["requested"],
        true
    );
    assert!(exit_payload(&local_events, "sigkill").unwrap()["signal"].is_string());
    for (id, _, _) in cases {
        let _ = local.kill(&sid(id));
    }
}

/// The app quits in sidecar mode: the one exit-time reap runs in the host and
/// takes the non-persisting sessions (a plain shell, an unhanded pool member);
/// the persisting one outlives the client and keeps filling the sidecar's ring.
/// Teardown bookkeeping reaches the app before the reap returns.
#[test]
fn quitting_reaps_the_non_persisting_and_the_rest_outlive_the_app() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let events = Arc::new(RecordingSink::default());
    let client =
        SessionServerClient::connect(root.path(), events.clone(), Arc::default(), None).unwrap();
    let story = spawn(
        &sidecar,
        &client,
        "while :; do echo TICK; sleep 0.2; done",
        registration("story-1", Some("SW-9"), false),
    );
    let plain = spawn(
        &sidecar,
        &client,
        "sleep 60",
        registration("plain-1", None, false),
    );
    let member = spawn(
        &sidecar,
        &client,
        "sleep 60",
        registration("__warm_pool_q", None, true),
    );

    client.reap_non_persisting().unwrap();
    let removed: Vec<String> = events
        .events()
        .into_iter()
        .filter_map(|event| match event {
            TerminalDomainEvent::SessionRemoved {
                session,
                reason: TeardownReason::Kill,
            } => Some(session.as_str().to_string()),
            _ => None,
        })
        .collect();
    assert!(
        removed.contains(&"plain-1".to_string()) && removed.contains(&"__warm_pool_q".to_string()),
        "teardown bookkeeping precedes the reply: {removed:?}"
    );
    assert!(!removed.contains(&"story-1".to_string()));
    assert!(wait_until(|| !alive(plain) && !alive(member)));

    drop(client);
    let ticks = || sidecar.text("story-1").matches("TICK").count();
    assert!(wait_until(|| ticks() >= 2));
    let seen = ticks();
    assert!(
        wait_until(|| ticks() > seen),
        "the ring keeps filling after the app left"
    );
    assert!(alive(story), "the persisting child outlives the app");
}

/// Every app on the Mac shares one sidecar. One app quitting reaps only the
/// non-persisting sessions it opened — its plain shell and its warm-pool
/// member, even across a reconnect — and never another app's, which stay
/// alive and hosted.
#[test]
fn quitting_reaps_only_the_sessions_this_app_opened() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let (ledger_a, ledger_b) = (Arc::<Ledger>::default(), Arc::<Ledger>::default());
    let events = Arc::new(RecordingSink::default());
    let first_a =
        SessionServerClient::connect(root.path(), events.clone(), ledger_a.clone(), None).unwrap();
    let b = SessionServerClient::connect(root.path(), events.clone(), ledger_b, None).unwrap();
    let a_plain = spawn(
        &sidecar,
        &first_a,
        "sleep 60",
        registration("a-plain", None, false),
    );
    let a_member = spawn(
        &sidecar,
        &first_a,
        "sleep 60",
        registration("__warm_pool_a", None, true),
    );
    let b_plain = spawn(
        &sidecar,
        &b,
        "sleep 60",
        registration("b-plain", None, false),
    );
    let b_member = spawn(
        &sidecar,
        &b,
        "sleep 60",
        registration("__warm_pool_b", None, true),
    );
    first_a.sever();
    assert!(wait_until(|| !first_a.is_alive()));
    let a = SessionServerClient::connect(root.path(), events.clone(), ledger_a, None).unwrap();

    a.reap_non_persisting().unwrap();
    assert!(wait_until(|| !alive(a_plain) && !alive(a_member)));
    assert_eq!(a.has_session(&sid("a-plain")), Ok(false));
    assert_eq!(a.has_session(&sid("__warm_pool_a")), Ok(false));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        alive(b_plain) && alive(b_member),
        "another app's sessions run on"
    );
    assert_eq!(
        b.alive_session_ids(&[sid("b-plain"), sid("__warm_pool_b")])
            .unwrap()
            .len(),
        2
    );
    assert!(!events.events().iter().any(|event| matches!(
        event,
        TerminalDomainEvent::SessionRemoved { session, .. }
            if session.as_str().starts_with("b-") || session.as_str() == "__warm_pool_b"
    )));
    b.kill(&sid("b-plain")).unwrap();
    b.kill(&sid("__warm_pool_b")).unwrap();
}

/// A request too large to frame is refused before any of it is sent: it
/// fails on its own and the connection stays up for the next request.
#[test]
fn an_oversized_request_fails_alone_and_keeps_the_connection() {
    let root = short_root();
    let _sidecar = TestServer::start_at(root.path());
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let huge = "x".repeat(codesense_host_proto::frame::MAX_FRAME_BYTES);
    let error = client.write(&sid("any"), huge.as_bytes()).unwrap_err();
    assert!(error.0.starts_with("send to the session server"), "{error}");
    assert!(client.is_alive());
    assert_eq!(
        client.is_idle(&sid("none")),
        Ok(true),
        "the next request is served"
    );
}

/// Write, resize, idle and kill reach the hosted session, and a runtime error
/// crosses with its exact text.
#[test]
fn session_verbs_reach_the_hosted_runtime() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let events = Arc::new(RecordingSink::default());
    let client =
        SessionServerClient::connect(root.path(), events.clone(), Arc::default(), None).unwrap();
    spawn(
        &sidecar,
        &client,
        "read line; stty size; printf 'GOT=%s\\n' \"$line\"; sleep 60",
        registration("io-1", None, false),
    );
    client
        .resize(&ViewerId::new("test"), &sid("io-1"), 132, 50)
        .unwrap();
    client.write(&sid("io-1"), b"ping\r").unwrap();
    assert!(wait_until(|| events.output("io-1").contains("GOT=ping")));
    assert!(events.output("io-1").contains("50 132"));
    assert!(
        wait_until(|| client.is_idle(&sid("io-1")) == Ok(false)),
        "sleep runs under the shell"
    );

    assert_eq!(
        client.write(&sid("gone"), b"x"),
        Err(PortUnavailable(
            "write_pty: session not found: gone".to_string()
        )),
        "the runtime's error text crosses the socket unchanged"
    );
    client.kill(&sid("io-1")).unwrap();
    assert!(wait_until(|| events.events().iter().any(|event| matches!(
        event,
        TerminalDomainEvent::SessionRemoved { session, reason: TeardownReason::Kill }
            if session.as_str() == "io-1"
    ))));
}

/// Everything `stream` delivered, in order, with each batch's watermark.
fn streamed(events: &RecordingSink, stream: StreamId) -> Vec<(String, u64)> {
    events
        .events()
        .into_iter()
        .filter_map(|event| match event {
            TerminalDomainEvent::StreamOutput {
                stream: s,
                data,
                seq,
            } if s == stream => Some((data, seq)),
            _ => None,
        })
        .collect()
}

/// SW-RM-4 4a.6: the connector-independent vertical contract. Both the local
/// UDS fixture and RM-3's admitted SSH stdio client invoke this exact body.
pub(crate) fn terminal_domain_connector_contract(
    client: Arc<SessionServerClient>,
    events: Arc<RecordingSink>,
    id: &str,
    intent: codesense_pty_core::spawn::SpawnIntent,
    setup: &str,
) -> String {
    connector_contract(client, events, id, intent, setup, false)
}

pub(crate) fn remote_tab_connector_contract(
    client: Arc<SessionServerClient>,
    events: Arc<RecordingSink>,
    id: &str,
    intent: codesense_pty_core::spawn::SpawnIntent,
    setup: &str,
) -> String {
    connector_contract(client, events, id, intent, setup, true)
}

/// Open a remote tab, run `command` in it and return its output once it
/// prints `marker`. Unlike the full contract it assumes nothing about what the
/// shell printed before the subscription, which a slow link reorders.
pub(crate) fn remote_tab_runs(
    client: Arc<SessionServerClient>,
    events: Arc<RecordingSink>,
    id: &str,
    intent: codesense_pty_core::spawn::SpawnIntent,
    command: &str,
    marker: &str,
) -> String {
    use codesense_pty_core::domain::{ClientBootId, ViewerId};
    use codesense_pty_core::layout::{OpenTab, Placement, WorkspaceDescriptor};

    let session = sid(id);
    let viewer = ViewerId::new(format!("viewer-{id}"));
    client
        .layout_snapshot(&viewer, &ClientBootId::new(format!("boot-{id}")))
        .unwrap();
    let spawn_client = client.clone();
    let spawn_id = id.to_string();
    client
        .open_remote_tab(
            OpenTab {
                viewer,
                workspace: WorkspaceKey::new(format!("runs-{id}")),
                session: session.clone(),
                descriptor: Some(WorkspaceDescriptor {
                    display_name: "remote tab".into(),
                    kind: "root".into(),
                    ..Default::default()
                }),
                name: None,
                role: None,
                tags: [("remoteRequestId".into(), format!("request-{id}"))].into(),
                placement: Placement::Tab,
                index: None,
                activate: true,
            },
            move || {
                spawn_client
                    .spawn(intent, registration(&spawn_id, None, false))
                    .map(|_| ())
            },
        )
        .unwrap();
    client.claim_writable_typed(&session).unwrap();
    client
        .subscribe(
            &session,
            &codesense_pty_core::domain::ViewportId::new(format!("vp-{id}")),
        )
        .unwrap();
    client
        .write(&session, format!("{command}\r").as_bytes())
        .unwrap();
    assert!(
        wait_until(|| events.output(id).contains(marker)),
        "{}",
        events.output(id)
    );
    events.output(id)
}

fn connector_contract(
    client: Arc<SessionServerClient>,
    events: Arc<RecordingSink>,
    id: &str,
    intent: codesense_pty_core::spawn::SpawnIntent,
    setup: &str,
    remote: bool,
) -> String {
    use codesense_pty_core::domain::{ClientBootId, ViewerId};
    use codesense_pty_core::layout::{OpenTab, Placement, WorkspaceDescriptor};

    let session = sid(id);
    let workspace = WorkspaceKey::new(format!("connector-{id}"));
    let viewer = ViewerId::new(format!("viewer-{id}"));
    let boot = ClientBootId::new(format!("boot-{id}"));
    client.layout_snapshot(&viewer, &boot).unwrap();
    let spawn_client = client.clone();
    let spawn_id = id.to_string();
    let cwd = intent.cwd.clone();
    let before = client.live_session_ids().unwrap();
    let request = OpenTab {
        viewer: viewer.clone(),
        workspace: workspace.clone(),
        session: session.clone(),
        descriptor: Some(WorkspaceDescriptor {
            display_name: "connector contract".into(),
            kind: "root".into(),
            ..Default::default()
        }),
        name: None,
        role: None,
        tags: [("remoteRequestId".into(), format!("request-{id}"))].into(),
        placement: Placement::Tab,
        index: None,
        activate: true,
    };
    if remote {
        client
            .open_remote_tab(request, move || {
                spawn_client
                    .spawn(intent, registration(&spawn_id, None, false))
                    .map(|_| ())
            })
            .unwrap();
        client.claim_writable_typed(&session).unwrap();
        client.claim_writable_typed(&session).unwrap();
    } else {
        client
            .open_tab(
                request,
                Box::new(move || {
                    spawn_client
                        .spawn(intent, registration(&spawn_id, None, false))
                        .map(|_| ())
                        .map_err(|failure| failure.message)
                }),
            )
            .unwrap();
    }
    let snapshot = client.layout_snapshot(&viewer, &boot).unwrap();
    assert!(snapshot.workspaces[&workspace]
        .tabs
        .iter()
        .any(|tab| tab.session == session));
    let subscription = client
        .subscribe(
            &session,
            &codesense_pty_core::domain::ViewportId::new(format!("vp-{id}")),
        )
        .unwrap();
    // SW-TH-9: the snapshot is the serialized terminal (`vt`) stamped with the
    // raw stream position it was taken at (`seq`), not a raw byte replay, so the
    // two lengths are unrelated once the shell has printed (a remote login has;
    // a fresh local shell may not have yet). The stream's continuity from `seq`
    // is checked by the watermark walk below.
    client
        .write(&session, format!("{setup}\r").as_bytes())
        .unwrap();
    assert!(wait_until(|| events.output(id).contains("READY")));
    client.write(&session, "héllo☃\r".as_bytes()).unwrap();
    let stream = subscription.stream_id;
    let joined = || {
        streamed(&events, stream)
            .into_iter()
            .map(|(data, _)| data)
            .collect::<String>()
    };
    assert!(wait_until(|| joined().contains("GOT=héllo☃")));
    let mut watermark = subscription.snapshot.seq;
    for (data, seq) in streamed(&events, stream) {
        assert_eq!(seq, watermark + data.len() as u64);
        watermark = seq;
    }
    let output = events.output(id);
    let pids = client
        .live_child_pids(std::slice::from_ref(&session))
        .unwrap();
    client.unsubscribe(stream).unwrap();
    if remote {
        assert!(output.contains(&format!("CWD={cwd}")), "{output}");
        assert_eq!(pids.len(), 1);
        let reattached = client
            .subscribe(
                &session,
                &codesense_pty_core::domain::ViewportId::new(format!("reattach-{id}")),
            )
            .unwrap();
        client.claim_writable_typed(&session).unwrap();
        assert_eq!(
            client
                .live_child_pids(std::slice::from_ref(&session))
                .unwrap(),
            pids
        );
        let snapshot = client.layout_snapshot(&viewer, &boot).unwrap();
        let tabs = &snapshot.workspaces[&workspace].tabs;
        assert_eq!(tabs.len(), 1);
        assert_eq!(
            tabs[0].tags.get("remoteRequestId"),
            Some(&format!("request-{id}"))
        );
        let mut expected = before.clone();
        expected.insert(session.clone());
        assert_eq!(client.live_session_ids().unwrap(), expected);
        let mut invalid = sh("cat");
        invalid.cwd = "/codesense-rm5-deliberately-missing-directory".into();
        assert!(client
            .spawn(invalid, registration(&format!("invalid-{id}"), None, false))
            .is_err());
        assert_eq!(client.live_session_ids().unwrap(), expected);
        client.unsubscribe(reattached.stream_id).unwrap();
    }
    client.kill(&session).unwrap();
    output
}

#[test]
fn terminal_domain_contract_runs_over_local_uds() {
    let sidecar = TestServer::start();
    let events = Arc::new(RecordingSink::default());
    let client =
        SessionServerClient::connect(sidecar.root(), events.clone(), Arc::default(), None).unwrap();
    sidecar.track("contract-uds");
    let _ = terminal_domain_connector_contract(
        client,
        events,
        "contract-uds",
        SpawnIntent {
            remote_launch: Default::default(),
            command: None,
            argv: None,
            shell: None,
            cwd: String::new(),
            cols: 80,
            rows: 24,
            shell_integration: false,
            starship_preset: None,
            api_port: None,
            env_remove: Vec::new(),
            env: Vec::new(),
            agent_env: Vec::new(),
            login_path: None,
        },
        "printf 'READY\\n'; read line; printf 'GOT=%s\\n' \"$line\"; sleep 60",
    );
}

/// A sidecar session attaches over the socket: the subscribe hands back the
/// snapshot, the viewport's stream resumes from its tail with no gap and no
/// duplicate — snapshot + stream is exactly the session's terminal.
/// Keystrokes cross as raw bytes, byte-identical. Unsubscribing detaches the
/// stream: no more output reaches it, while the session keeps running.
#[test]
fn a_sidecar_session_attaches_without_gap_or_duplicate() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let events = Arc::new(RecordingSink::default());
    let client =
        SessionServerClient::connect(root.path(), events.clone(), Arc::default(), None).unwrap();
    spawn(
        &sidecar,
        &client,
        "printf 'BEFORE\\n'; read a; printf 'AFTER=%s\\n' \"$a\"; read b; printf 'LATE=%s\\n' \"$b\"; sleep 60",
        registration("att-1", None, false),
    );
    let id = sid("att-1");
    assert!(wait_until(|| sidecar.text("att-1").contains("BEFORE")));
    let sub = client
        .subscribe(&id, &codesense_pty_core::domain::ViewportId::new("vp"))
        .unwrap();
    assert!(sub.snapshot.vt.contains("BEFORE"), "{:?}", sub.snapshot);
    assert_eq!(sub.snapshot.seq, sub.snapshot.vt.len() as u64);

    client.write(&id, "\u{e9}\u{2603}\r".as_bytes()).unwrap();
    let stream = sub.stream_id;
    let joined = || -> String {
        streamed(&events, stream)
            .into_iter()
            .map(|(d, _)| d)
            .collect()
    };
    assert!(wait_until(|| joined().contains("AFTER=\u{e9}\u{2603}")));
    assert!(wait_until(|| format!("{}{}", sub.snapshot.vt, joined())
        .replace("\r\n", "\n")
        == sidecar.text("att-1")));
    let batches = streamed(&events, stream);
    let mut watermark = sub.snapshot.seq;
    for (data, seq) in &batches {
        assert_eq!(
            *seq,
            watermark + data.len() as u64,
            "contiguous from the snapshot"
        );
        watermark = *seq;
    }

    assert_eq!(client.unsubscribe(stream), Ok(true));
    assert_eq!(client.unsubscribe(stream), Ok(false), "already detached");
    client.write(&id, b"two\r").unwrap();
    assert!(wait_until(|| events.output("att-1").contains("LATE=two")));
    assert!(
        !joined().contains("LATE"),
        "a detached stream gets no more output"
    );
}

/// A lost connection detaches its viewports in the host (see the sidecar's
/// own test); the viewport then re-attaches over the new connection with the same snapshot + register race
/// check: the new snapshot and the new stream are again exactly the terminal.
#[test]
fn a_viewport_reattaches_without_gap_after_a_reconnect() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let events = Arc::new(RecordingSink::default());
    let state = SessionServerState::default();
    let first = state
        .connect_or_launch(root.path(), events.clone(), no_launch)
        .unwrap();
    spawn(
        &sidecar,
        &first,
        "printf 'BEFORE\\n'; read a; printf 'AFTER=%s\\n' \"$a\"; sleep 60",
        registration("re-1", None, false),
    );
    let id = sid("re-1");
    let viewport = codesense_pty_core::domain::ViewportId::new("vp");
    assert!(wait_until(|| sidecar.text("re-1").contains("BEFORE")));
    first.subscribe(&id, &viewport).unwrap();
    first.sever();
    assert!(wait_until(|| !first.is_alive()));

    let second = state.reconnected(root.path()).expect("reconnected");
    let again = second.subscribe(&id, &viewport).unwrap();
    assert!(again.snapshot.vt.contains("BEFORE"));
    second.write(&id, b"x\r").unwrap();
    let joined = || -> String {
        streamed(&events, again.stream_id)
            .into_iter()
            .map(|(data, _)| data)
            .collect()
    };
    assert!(wait_until(|| joined().contains("AFTER=x")));
    assert!(wait_until(|| format!("{}{}", again.snapshot.vt, joined())
        .replace("\r\n", "\n")
        == sidecar.text("re-1")));
}

/// The submit protocols run where the session lives: each writes its line into
/// the hosted session and reports the outcome; a missing session is an error.
#[test]
fn the_submit_protocols_reach_the_hosted_session() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let events = Arc::new(RecordingSink::default());
    let client =
        SessionServerClient::connect(root.path(), events.clone(), Arc::default(), None).unwrap();
    spawn(
        &sidecar,
        &client,
        "printf '$ '; while read line; do printf 'GOT=%s\\n$ ' \"$line\"; done",
        registration("submit-1", None, false),
    );
    assert_eq!(
        client
            .submit_shell_line(&sid("submit-1"), "shell-line", 100, 5000)
            .map(|outcome| outcome.submitted),
        Ok(true),
        "the settled prompt takes the line"
    );
    assert!(wait_until(|| events
        .output("submit-1")
        .contains("GOT=shell-line")));
    assert!(client
        .submit_line(&sid("submit-1"), "agent-line", 100, 100, 5000, 1)
        .is_ok());
    assert!(wait_until(|| events
        .output("submit-1")
        .contains("GOT=agent-line")));
    assert!(client
        .submit_shell_line(&sid("gone"), "x", 100, 1000)
        .is_err());
}

/// A mock host's hello for `instance`: this protocol, hosting terminals.
fn mock_hello(instance: String) -> Hello {
    Hello {
        proto_major: PROTO_MAJOR,
        instance_id: instance,
        capabilities: vec![Capability::Terminals],
        pid: 1,
        emulator: Some(codesense_host_proto::EmulatorHello::current()),
        ..Hello::default()
    }
}

/// Greet a client as a host would: `hello`, then a census of
/// `live_sessions`; then read the client's hello.
fn greet_client(stream: &UnixStream, hello: &Hello, live_sessions: u32) -> PeerHello {
    write_frame(&mut &*stream, &hello.frame().unwrap()).unwrap();
    write_frame(&mut &*stream, &Census { live_sessions }.frame()).unwrap();
    read_hello(&mut &*stream).unwrap()
}

/// The next call on `stream`, if any.
fn next_call(stream: &UnixStream) -> Option<(u64, Request)> {
    let frame = read_frame(&mut &*stream).unwrap()?;
    match frame.decode_json::<Message>().unwrap() {
        Message::Call { rid, request } => Some((rid, request)),
        other => panic!("unexpected {other:?}"),
    }
}

/// Push `event` on `stream` through `egress`, as the sidecar does.
fn push(stream: &UnixStream, egress: &mut Egress, event: TerminalDomainEvent) {
    for frame in egress.frames(event).unwrap() {
        write_frame(&mut &*stream, &frame).unwrap();
    }
}

/// A mock sidecar for `root`: greets each connection, answers its
/// listen call, then hands its next call's `rid` to `serve` (which may
/// reply). Returns the calls served.
pub(crate) fn mock_sidecar(
    root: &Path,
    connections: usize,
    serve: impl Fn(usize, &UnixStream, u64) + Send + 'static,
) -> std::thread::JoinHandle<Vec<Request>> {
    let listener = UnixListener::bind(socket_path(root)).unwrap();
    let hello = mock_hello(instance_id(root));
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        for n in 0..connections {
            let (stream, _) = listener.accept().unwrap();
            assert!(matches!(
                greet_client(&stream, &hello, 0),
                PeerHello::Compatible(_)
            ));
            while let Some((rid, request)) = next_call(&stream) {
                if matches!(request, Request::Listen) {
                    answer(&stream, rid, Ok(Reply::Done));
                    continue;
                }
                seen.push(request);
                serve(n, &stream, rid);
                break;
            }
        }
        seen
    })
}

pub(crate) fn answer(stream: &UnixStream, rid: u64, result: Result<Reply, String>) {
    let frame = Frame::control(&Message::Reply { rid, result }).unwrap();
    write_frame(&mut &*stream, &frame).unwrap();
}

/// A mock host for `root` that greets one connection with `hello` and a
/// census of `live_sessions`, then closes it. Returns the client's hello.
fn mock_host(
    root: &Path,
    hello: Hello,
    live_sessions: u32,
) -> std::thread::JoinHandle<Result<PeerHello, String>> {
    let listener = UnixListener::bind(socket_path(root)).unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        write_frame(&mut &stream, &hello.frame().unwrap()).unwrap();
        write_frame(&mut &stream, &Census { live_sessions }.frame()).unwrap();
        read_hello(&mut &stream)
    })
}

/// This instance's sidecar one major back, or at this major running another
/// emulator, is refused at the handshake like any incompatible one: the
/// restart state, no client bytes, no subscribe.
#[test]
fn a_previous_major_or_another_emulator_is_refused() {
    for (hello_of, reason) in [
        (
            (|root: &Path| Hello {
                proto_major: PROTO_MAJOR - 1,
                emulator: None,
                ..mock_hello(instance_id(root))
            }) as fn(&Path) -> Hello,
            codesense_host_proto::SkewReason::ProtoMajor {
                peer: PROTO_MAJOR - 1,
            },
        ),
        (
            |root: &Path| Hello {
                emulator: Some(codesense_host_proto::EmulatorHello {
                    xterm_version: "5.5.0".into(),
                }),
                ..mock_hello(instance_id(root))
            },
            codesense_host_proto::SkewReason::EmulatorVersion {
                peer: Some("5.5.0".into()),
            },
        ),
    ] {
        let root = short_root();
        let mock = mock_host(root.path(), hello_of(root.path()), 2);
        let state = SessionServerState::default();
        let error = state
            .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
            .err()
            .expect("an incompatible sidecar is not used");
        match &error {
            ConnectError::Incompatible(skew) => {
                assert_eq!(skew.reason, reason);
                assert_eq!(skew.live_sessions, Some(2));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(error
            .to_string()
            .starts_with("restart terminal server (ends 2 sessions)"));
        assert!(
            mock.join().unwrap().is_err(),
            "the incompatible host received client bytes"
        );
    }
}

#[test]
fn heartbeat_uses_its_five_second_timeout_and_exact_request_correlation() {
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let hello = mock_hello(instance_id(root.path()));
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        assert!(matches!(
            greet_client(&stream, &hello, 0),
            PeerHello::Compatible(_)
        ));
        let (listen_rid, listen) = next_call(&stream).unwrap();
        assert_eq!(listen, Request::Listen);
        answer(&stream, listen_rid, Ok(Reply::Done));

        let (heartbeat_rid, heartbeat) = next_call(&stream).unwrap();
        assert_eq!(heartbeat, Request::LiveSessionIds);
        answer(
            &stream,
            heartbeat_rid + 10_000,
            Ok(Reply::Sessions {
                sessions: [sid("unrelated")].into_iter().collect(),
            }),
        );
        std::thread::sleep(Duration::from_secs(4));
        answer(
            &stream,
            heartbeat_rid,
            Ok(Reply::Sessions {
                sessions: [sid("real")].into_iter().collect(),
            }),
        );

        let (_, heartbeat) = next_call(&stream).unwrap();
        assert_eq!(heartbeat, Request::LiveSessionIds);
        std::thread::sleep(Duration::from_secs(6));
    });
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(client.heartbeat(Duration::from_secs(5)), Ok(()));
    assert!(started.elapsed() >= Duration::from_secs(4));
    let started = std::time::Instant::now();
    assert!(client.heartbeat(Duration::from_secs(5)).is_err());
    assert!(started.elapsed() >= Duration::from_millis(4_900));
    server.join().unwrap();
}

/// This instance's sidecar at another protocol major is reported, not fallen
/// back from: the error names the sessions a restart would end, and no launch
/// is attempted over it.
#[test]
fn rm4c_uds_older_major_has_exact_census_and_zero_client_frames() {
    let root = short_root();
    let older = Hello {
        proto_major: PROTO_MAJOR - 2,
        ..mock_hello(instance_id(root.path()))
    };
    let mock = mock_host(root.path(), older, 3);
    let state = SessionServerState::default();
    let launched = std::cell::Cell::new(false);
    let error = state
        .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), |_| {
            launched.set(true);
            Ok(())
        })
        .err()
        .expect("an incompatible sidecar is not used");
    match &error {
        ConnectError::Incompatible(skew) => {
            assert_eq!(skew.live_sessions, Some(3));
            assert_eq!(skew.host_major(), PROTO_MAJOR - 2);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(error
        .to_string()
        .starts_with("restart terminal server (ends 3 sessions)"));
    assert!(!launched.get(), "never launched over");
    assert!(
        matches!(
            state.connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch),
            Err(ConnectError::Incompatible(_))
        ),
        "reported again without a new handshake inside the backoff window"
    );
    assert!(
        mock.join().unwrap().is_err(),
        "older host received client bytes"
    );
}

/// A host that does not advertise terminals is not used for them: the
/// capability is missing, so the sidecar is refused and terminals stay
/// in-process.
#[test]
fn a_host_without_terminals_is_not_used_for_them() {
    let root = short_root();
    let bare = Hello {
        capabilities: Vec::new(),
        ..mock_hello(instance_id(root.path()))
    };
    let mock = mock_host(root.path(), bare, 0);
    match SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    ) {
        Err(ConnectError::Foreign(message)) => {
            assert!(message.contains("does not host terminals"), "{message}")
        }
        other => panic!("unexpected {:?}", other.map(|_| ())),
    }
    assert!(
        mock.join().unwrap().is_err(),
        "refused host received client bytes"
    );
}

/// Every Rust source under `path` (a file or a directory, recursively).
fn rust_sources(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            rust_sources(&entry.unwrap().path(), out);
        }
    } else if path.extension().is_some_and(|ext| ext == "rs") {
        out.push(path.to_path_buf());
    }
}

/// One codec: nothing on the terminal-server socket path frames messages
/// itself — no newline-delimited JSON, direct JSON encoding or raw socket
/// writes — outside `codesense-host-proto`. The path is every source of the
/// sidecar crate, the terminal service's protocol, the app's terminal client
/// and its backend resolution. (The MCP bridge socket is a separate protocol.)
#[test]
fn the_host_socket_path_frames_only_through_host_proto() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    for path in [
        "crates/codesense-session-server",
        "crates/codesense-pty-core/src/protocol.rs",
        "src/terminal/session_server.rs",
        "src/terminal/session_server",
        "src/commands/pty_backend.rs",
    ] {
        rust_sources(&manifest.join(path), &mut sources);
    }
    assert!(
        sources.len() >= 10,
        "the socket path was found: {sources:?}"
    );
    let forbidden = [
        ["encode", "_line"].concat(),
        ["decode", "_line"].concat(),
        ["read", "_line"].concat(),
        ["read", "_until"].concat(),
        [".lines", "()"].concat(),
        ["BufRead", ","].concat(),
        ["BufRead", ";"].concat(),
        ["BufRead", "}"].concat(),
        ["serde", "_json"].concat(),
        ["write", "_all("].concat(),
    ];
    // The tokens a file may use without framing anything: the SSH bootstrap
    // reads command output line by line, a test cuts an upload short over
    // `ssh` stdin, and the `attach` relay writes raw bytes by design (its
    // transparency tests pin that it never frames). The SSH connector and the
    // Add-machine checklist read `ssh` output line by line too, and the
    // askpass answers on its own one-shot socket, never the host's.
    let write_all = ["write", "_all("].concat();
    let lines = [".lines", "()"].concat();
    let json = ["serde", "_json"].concat();
    let until = ["read", "_until"].concat();
    let buf_read = ["BufRead", ","].concat();
    let line_read = ["read", "_line"].concat();
    let allowed = [
        ("src/install.rs", &lines),
        ("src/survival.rs", &lines),
        ("src/ssh.rs", &lines),
        ("src/check.rs", &lines),
        ("tests/clean_direction.rs", &lines),
        ("tests/remote_host.rs", &write_all),
        ("src/attach.rs", &write_all),
        ("src/askpass.rs", &write_all),
        // The isolated disk receipt writer accepts a path, never a byte stream.
        ("src/replacement_evidence.rs", &json),
        ("src/replacement_evidence.rs", &write_all),
        ("src/replacement_data.rs", &lines),
        ("src/private_account_data.rs", &json),
        // The private-account bootstrap streams its install bundle over `ssh`
        // stdin; the restricted native entry reads that bundle's header line,
        // checks its digest manifest line by line and writes the files it
        // installs. Neither touches the host socket.
        ("src/private_account.rs", &write_all),
        ("src/private_account_native.rs", &write_all),
        ("src/private_account_native.rs", &lines),
        ("src/private_account_native.rs", &until),
        ("src/private_account_native.rs", &buf_read),
        // The desktop's reverse MCP relay writes each `MCP_DATA` payload
        // verbatim into the in-process duplex its MCP server reads; everything
        // it sends back to the host leaves as a `Frame` through the client's
        // writer. The sidecar half's broker-socket codec lives in
        // `codesense_host_proto::reverse_mcp`.
        ("session_server/reverse_mcp.rs", &write_all),
        // The private Git workspace store keeps its receipts, ownership
        // markers and file-operation ledger as JSON files, writes uploaded
        // bundle chunks to disk, and splits `git for-each-ref` output and
        // managed-block text into lines. The host hands it decoded requests; it
        // never touches the socket (guarded below).
        ("src/remote_workspace.rs", &json),
        ("src/remote_workspace.rs", &write_all),
        ("src/remote_workspace.rs", &lines),
        ("remote_workspace/files.rs", &json),
        ("remote_workspace/files.rs", &lines),
        ("remote_workspace/files_tests.rs", &json),
        // SW-XS-9: the base update splits `git status` output and the managed
        // exclude block into lines; it receives decoded requests only.
        ("remote_workspace/base_update.rs", &lines),
        // Pinned-export receipts are disk JSON; list-heads is Git command output.
        // The workspace-wide no-transport assertion below covers this module too.
        ("remote_workspace/export.rs", &json),
        ("remote_workspace/export.rs", &lines),
        // A test reads the port its local HTTPS fixture prints on stdout.
        ("tests/askpass.rs", &line_read),
        ("tests/askpass.rs", &buf_read),
        // Marker identity values and round-trip fixtures are JSON data, not
        // encoding performed by the host socket reader or writer.
        ("src/machine_owner.rs", &json),
        ("src/private_account_native.rs", &json),
        ("codesense-pty-core/src/protocol.rs", &json),
        // The usage receiver is its own loopback HTTP listener for the agent CLIs' telemetry
        // export, never the host socket; its spool is JSON lines on disk, which the host reads back
        // and answers as a decoded `Reply`.
        ("src/usage_receiver.rs", &line_read),
        ("src/usage_receiver.rs", &lines),
        ("src/usage_receiver.rs", &buf_read),
        ("src/usage_receiver.rs", &json),
        ("src/usage_receiver.rs", &write_all),
        ("src/usage_spool.rs", &lines),
        ("src/usage_spool.rs", &buf_read),
        ("src/usage_spool.rs", &json),
        ("src/usage_spool.rs", &write_all),
        // The MCP-bridge disconnect log appends text lines to a file on disk
        // and its tests read them back; it never touches the host socket.
        ("src/disconnect_log.rs", &write_all),
        ("src/disconnect_log.rs", &lines),
        // Batch receipt JSON and gzip chunks are confined disk I/O. The
        // storage-only no-transport guard below also covers these modules.
        ("remote_workspace/context_batch.rs", &json),
        ("remote_workspace/context_batch.rs", &write_all),
        (
            "remote_workspace/context_batch.rs",
            &["BufRead", ";"].concat(),
        ),
        // SW-XS-6: the manifest's hash cache is a JSON file on disk, replaced
        // by tmp + rename; its tests write workspace files. The storage-only
        // no-transport guard below covers both.
        ("remote_workspace/manifest.rs", &json),
        ("remote_workspace/manifest.rs", &write_all),
        ("remote_workspace/manifest_tests.rs", &json),
        ("remote_workspace/manifest_tests.rs", &write_all),
        ("remote_workspace/context_batch_tests.rs", &json),
        // The session wake store is JSON data on disk (SW-AGR-4); the module
        // holds no transport.
        ("session_wake/store.rs", &json),
        ("session_wake/store.rs", &lines),
        ("session_wake/store.rs", &write_all),
        ("remote_workspace/context_batch_tests.rs", &write_all),
        // SW-XS-26: a batch member's staged bytes and the bench's gzip input
        // are file writes; the crash tests read retained receipts as JSON.
        ("remote_workspace/files.rs", &write_all),
        ("remote_workspace/context_batch_bench.rs", &write_all),
        ("remote_workspace/context_batch_xs26_tests.rs", &json),
        // Native CLI configuration/stdout and its separate supervisor control
        // channel are not the terminal host socket. Host traffic remains Frame.
        ("src/native_launch.rs", &json),
        ("src/native_launch.rs", &lines),
        // The generated per-session Agent settings file is written to disk.
        ("src/native_launch.rs", &write_all),
        // Agent startup settings are JSON values carried in decoded requests and
        // written to settings-file fixtures, not host socket framing.
        ("tests/activity.rs", &json),
        ("session_server/mcp_input_delivery_tests.rs", &json),
        ("src/owned_supervisors.rs", &json),
        ("src/owned_supervisors.rs", &write_all),
        ("src/reverse_mcp.rs", &json),
        ("src/reverse_mcp.rs", &write_all),
        // SW-AGR-8: the machine's own answers to an agent's MCP JSON-RPC on
        // the broker socket while no desktop relays it (never the host
        // socket), and their test fixtures.
        ("routine_arm/local_mcp.rs", &json),
        ("routine_arm/tests.rs", &json),
        // SW-AGR-11: the machine's offline answers and outbox are agent MCP
        // JSON-RPC on the broker socket (never the host socket); the outbox
        // delivery reads its own in-process stream's answers line by line.
        ("src/offline_mcp.rs", &json),
        // ...and keeps queued calls and its lost-call log on disk.
        ("src/offline_mcp.rs", &lines),
        ("src/reverse_mcp.rs", &line_read),
        ("src/reverse_mcp.rs", &["BufRead", ";"].concat()),
        ("tests/remote_stdio_lifecycle.rs", &json),
        ("tests/remote_stdio_lifecycle.rs", &lines),
        ("tests/remote_stdio_lifecycle.rs", &write_all),
        // Task-owned WebSocket handshake/frames speak to the native execution
        // runtime; this module receives already-decoded host requests.
        ("remote_workspace/executor.rs", &until),
        ("remote_workspace/executor.rs", &buf_read),
        ("remote_workspace/executor.rs", &write_all),
    ];
    let evidence_path =
        manifest.join("crates/codesense-session-server/src/replacement_evidence.rs");
    let evidence = std::fs::read_to_string(evidence_path).unwrap();
    assert!(evidence.contains("fn write(root: &Path, kind: &str, receipt: &Reply)"));
    // Both exceptions remain data-only: neither may acquire transport access.
    let data_path = manifest.join("crates/codesense-session-server/src/replacement_data.rs");
    let data = std::fs::read_to_string(data_path).unwrap();
    let marker_data = std::fs::read_to_string(
        manifest.join("crates/codesense-session-server/src/private_account_data.rs"),
    )
    .unwrap();
    let forbidden_access = [
        "UnixStream",
        "TcpStream",
        "UdpSocket",
        "std::net",
        "std::os",
        "tokio::",
        "libc::",
        "RawFd",
        "RawHandle",
        "Transport",
        "Socket",
        "crate::host",
        "crate::server",
    ];
    let transport_access = |text: &str| forbidden_access.iter().any(|token| text.contains(token));
    for (name, text) in [
        ("disk evidence", &evidence),
        ("replacement data", &data),
        ("private marker data", &marker_data),
    ] {
        assert!(!transport_access(text), "{name} gained transport access");
        for token in forbidden_access {
            assert!(
                transport_access(&format!("{text}\n{token}")),
                "{name} guard missed {token}"
            );
        }
    }
    // The workspace store runs git and owns files by descriptor, so it may use
    // `std::os` and `libc`, but never a stream, socket or the host itself.
    let mut workspace_sources = Vec::new();
    let store = manifest.join("crates/codesense-session-server/src/remote_workspace");
    rust_sources(&store.with_extension("rs"), &mut workspace_sources);
    rust_sources(&store, &mut workspace_sources);
    // The native task executor owns the admitted WebSocket transport; it is
    // covered by the socket-codec scan above, not this storage-only boundary.
    let executor_transport = store.join("executor.rs");
    assert!(workspace_sources.contains(&executor_transport));
    workspace_sources.retain(|path| path != &executor_transport);
    assert!(workspace_sources.len() >= 2, "{workspace_sources:?}");
    let workspace = workspace_sources
        .iter()
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect::<String>();
    for token in [
        "UnixStream",
        "TcpStream",
        "UdpSocket",
        "std::net",
        "tokio::",
        "Transport",
        "Socket",
        "crate::host",
        "crate::server",
    ] {
        assert!(
            !workspace.contains(token),
            "the workspace store gained transport access: {token}"
        );
    }
    for token in ["std::fs", "std::io"] {
        assert!(
            !data.contains(token) && !marker_data.contains(token),
            "pure replacement/marker data gained {token}"
        );
    }
    let violations = |source: &Path, text: &str| -> Vec<String> {
        forbidden
            .iter()
            .filter(|token| {
                text.contains(token.as_str())
                    && !allowed
                        .iter()
                        .any(|(file, allowed)| source.ends_with(file) && *allowed == *token)
            })
            .cloned()
            .collect()
    };
    // Calibrate against every forbidden primitive: transport-bearing files
    // never inherit the narrowly named data helpers' exceptions.
    for speaker in ["src/host.rs", "src/replace.rs"] {
        for token in &forbidden {
            assert_eq!(violations(Path::new(speaker), token), vec![token.clone()]);
        }
    }
    let mut offenders = Vec::new();
    for source in &sources {
        let text = std::fs::read_to_string(source).unwrap();
        for token in violations(source, &text) {
            offenders.push(format!("{}: `{token}`", source.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "socket framing outside codesense-host-proto:\n{}",
        offenders.join("\n")
    );
    for speaker in [
        "crates/codesense-pty-core/src/protocol.rs",
        "crates/codesense-session-server/src/server.rs",
        "crates/codesense-session-server/src/host.rs",
        "src/terminal/session_server.rs",
    ] {
        let text = std::fs::read_to_string(manifest.join(speaker)).unwrap();
        assert!(
            text.contains("codesense_host_proto"),
            "{speaker} speaks codesense-host-proto"
        );
    }
}

#[test]
fn a_lost_spawn_reply_falls_open_only_once_the_cancel_is_confirmed() {
    for confirm in [true, false] {
        let root = short_root();
        let mock = mock_sidecar(root.path(), 2, move |n, stream, rid| match n {
            // The first connection goes away without answering the spawn.
            0 => stream.shutdown(std::net::Shutdown::Both).unwrap(),
            _ if confirm => answer(stream, rid, Ok(Reply::Done)),
            _ => {}
        });
        let client = SessionServerClient::connect(
            root.path(),
            Arc::new(RecordingSink::default()),
            Arc::default(),
            None,
        )
        .unwrap();
        let owners = crate::commands::pty_host::LocalEffectOwners::default();
        let claim = owners
            .reserve_spawn(
                crate::commands::machines::RuntimeMemberTarget {
                    member: crate::commands::machines::RuntimeMemberAddress::local(
                        RuntimeMemberSlot::Sidecar,
                    ),
                    admission_revision: crate::commands::machines::RuntimeCounter::new(1),
                    namespace: client.namespace().into(),
                },
                "twice",
                false,
            )
            .unwrap();
        let failure = client
            .spawn(sh("true"), registration("twice", None, false))
            .expect_err("the reply was lost");
        assert_eq!(failure.may_host_elsewhere, confirm, "{failure:?}");
        assert_eq!(
            failure.disposition,
            if confirm {
                SpawnDisposition::CancelledUndrained
            } else {
                SpawnDisposition::Unknown
            }
        );
        owners.spawn_failed(&claim, failure.disposition);
        assert!(
            owners
                .reserve_spawn(claim.target.clone(), "twice", false)
                .is_err(),
            "a cancel reply is not original Exit plus Removed ingress"
        );
        let seen = mock.join().unwrap();
        assert!(
            matches!(seen.as_slice(), [Request::Spawn { .. }, Request::CancelSpawn { session }] if session.as_str() == "twice"),
            "{seen:?}"
        );
    }
}

/// A spawn cancelled because the session was closed while it opened is final:
/// the caller must not host it elsewhere; a spawn the runtime refused may.
#[test]
fn a_cancelled_spawn_is_final_and_a_refused_one_falls_open() {
    for (reply, may_host_elsewhere) in [
        (Ok(Reply::Cancelled), false),
        (Err("Failed to open PTY: no pty".to_string()), true),
    ] {
        let root = short_root();
        let reply = Mutex::new(Some(reply));
        let mock = mock_sidecar(root.path(), 1, move |_, stream, rid| {
            answer(stream, rid, lock(&reply).take().unwrap())
        });
        let client = SessionServerClient::connect(
            root.path(),
            Arc::new(RecordingSink::default()),
            Arc::default(),
            None,
        )
        .unwrap();
        let owners = crate::commands::pty_host::LocalEffectOwners::default();
        let claim = owners
            .reserve_spawn(
                crate::commands::machines::RuntimeMemberTarget {
                    member: crate::commands::machines::RuntimeMemberAddress::local(
                        RuntimeMemberSlot::Sidecar,
                    ),
                    admission_revision: crate::commands::machines::RuntimeCounter::new(1),
                    namespace: client.namespace().into(),
                },
                "closing",
                false,
            )
            .unwrap();
        let failure = client
            .spawn(sh("true"), registration("closing", None, false))
            .expect_err("not hosted");
        assert_eq!(
            failure.may_host_elsewhere, may_host_elsewhere,
            "{failure:?}"
        );
        assert_eq!(
            failure.disposition,
            if may_host_elsewhere {
                SpawnDisposition::RejectedUndrained
            } else {
                SpawnDisposition::CancelledUndrained
            }
        );
        owners.spawn_failed(&claim, failure.disposition);
        assert!(
            owners
                .reserve_spawn(claim.target.clone(), "closing", false)
                .is_err(),
            "runtime rejection can follow partial registration; do not fabricate drain"
        );
        mock.join().unwrap();
    }
}

/// A terminal server from before the respawn verb is never sent it: the
/// respawn is refused with the plain restart-the-server message.
#[test]
fn a_respawn_on_a_terminal_server_older_than_the_verb_names_the_restart() {
    let root = short_root();
    let mock = mock_sidecar(root.path(), 1, |_, _, _| {});
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let refused = client.respawn_tab(&sid("old-server"), Box::new(|| panic!("never spawns")));
    assert_eq!(
        refused.map_err(|e| e.to_string()),
        Err(RESPAWN_NEEDS_UPDATED_SERVER.to_string())
    );
    drop(client);
    assert!(mock.join().unwrap().is_empty(), "nothing was sent");
}

/// A connected client of a mock terminal server that greets with `hello`, answers every call
/// (a spawn as spawned), and returns the calls it was sent once the client is gone.
fn client_of_mock(
    root: &Path,
    hello: impl FnOnce(Hello) -> Hello,
) -> (
    Arc<SessionServerClient>,
    std::thread::JoinHandle<Vec<Request>>,
) {
    let listener = UnixListener::bind(socket_path(root)).unwrap();
    let hello = hello(mock_hello(instance_id(root)));
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        greet_client(&stream, &hello, 0);
        let mut seen = Vec::new();
        while let Some((rid, request)) = next_call(&stream) {
            let reply = match &request {
                Request::Spawn { .. } => Reply::Spawned { pid: Some(7) },
                Request::RoutineArm { .. } => Reply::RoutineArmAnswer {
                    answer: Box::new(codesense_pty_core::routine_arm::ArmAnswer::Disarmed {
                        was_armed: false,
                        last_fired_ms: None,
                    }),
                },
                _ => Reply::Done,
            };
            if !matches!(request, Request::Listen) {
                seen.push(request);
            }
            answer(&stream, rid, Ok(reply));
        }
        seen
    });
    let client = SessionServerClient::connect(
        root,
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    (client, server)
}

/// A routine arm whose launch starts an agent that sets `env`.
fn arm_with_agent_env(env: &[(&str, &str)]) -> codesense_pty_core::routine_arm::RoutineArm {
    use codesense_pty_core::routine_arm::*;
    RoutineArm {
        limits: None,
        key: "nightly".into(),
        epoch: 1,
        revision: 1,
        name: "nightly".into(),
        mode: ArmMode::Wake,
        schedules: vec![ArmSchedule {
            cron: "* * * * *".into(),
            tz: "UTC".into(),
        }],
        task: "triage".into(),
        target: ArmTarget {
            agent: "alderco/riverside/reviewer".into(),
            workspace_id: "objects".into(),
            cwd: "/home/dev/work".into(),
            owner: None,
            allow_wake: false,
        },
        max_wait_ms: 60_000,
        overlap: ArmOverlap::Coalesce,
        missed: ArmMissed::Skip,
        launch: Some(ArmLaunch {
            launch: codesense_host_proto::agent_cli::NativeLaunch::Cli(
                codesense_host_proto::agent_cli::AgentCli::Claude,
            ),
            agent_startup: codesense_host_proto::reverse_mcp::AgentStartup {
                args: vec![],
                settings: "{}".parse().unwrap(),
                prompt: None,
                env: env
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            },
            prepared_paths: vec![],
            prepared_digest: String::new(),
            workspace: codesense_pty_core::domain::WorkspaceKey::new("ws"),
            descriptor: None,
            tags: Default::default(),
        }),
        originator: codesense_pty_core::session_wake::Originator {
            desktop: String::new(),
            routine_host: Some("mac".into()),
        },
        author: String::new(),
        max_runtime_ms: 0,
        last_fired_ms: None,
        last_run_summary: None,
        last_run_at_ms: None,
        source_digest: String::new(),
    }
}

/// A terminal server that would drop a saved Agent's own variables — one before
/// `AGENT_ENV_PROTO_MINOR`, or one at it that does not advertise `AgentEnvV1` — is never sent
/// them: a spawn and an arm that carry them are refused by name before anything is sent, and the
/// spawn may not fall open elsewhere. A spawn and an arm that set none still go to it, and a
/// server with the capability takes them.
#[test]
fn a_terminal_server_without_agent_env_refuses_an_agent_that_sets_variables() {
    let with_env = || {
        let mut intent = sh("true");
        intent.agent_env = vec![("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), "1".into())];
        intent
    };
    let old: [fn(Hello) -> Hello; 2] = [
        |hello| Hello {
            proto_minor: AGENT_ENV_PROTO_MINOR - 1,
            capabilities: vec![
                Capability::Terminals,
                Capability::RoutineArmV1,
                Capability::AgentEnvV1,
            ],
            ..hello
        },
        |hello| Hello {
            proto_minor: AGENT_ENV_PROTO_MINOR,
            capabilities: vec![Capability::Terminals, Capability::RoutineArmV1],
            ..hello
        },
    ];
    for hello in old {
        let root = short_root();
        let (client, server) = client_of_mock(root.path(), hello);
        assert!(!client.supports_agent_env());
        let refused = client
            .spawn(with_env(), registration("env-old", None, false))
            .expect_err("refused");
        assert_eq!(refused.message, AGENT_ENV_NEEDS_UPDATED_SERVER);
        assert!(!refused.may_host_elsewhere);
        assert_eq!(refused.disposition, SpawnDisposition::NeverStarted);
        assert_eq!(
            client.routine_arm(arm_with_agent_env(&[("FOO", "bar")])),
            Err(AGENT_ENV_NEEDS_UPDATED_SERVER.to_string())
        );
        assert_eq!(
            client.require_agent_env(true),
            Err(AGENT_ENV_NEEDS_UPDATED_SERVER.to_string())
        );
        assert_eq!(client.require_agent_env(false), Ok(()));
        // An agent that sets none keeps working on it.
        assert_eq!(
            client
                .spawn(sh("true"), registration("env-none", None, false))
                .map_err(|f| f.message),
            Ok(Some(7))
        );
        assert!(client.routine_arm(arm_with_agent_env(&[])).is_ok());
        drop(client);
        let seen = server.join().unwrap();
        assert!(
            matches!(
                &seen[..],
                [Request::Spawn { intent, .. }, Request::RoutineArm { .. }]
                    if intent.agent_env.is_empty()
            ),
            "only the launches without variables were sent: {seen:?}"
        );
    }
    let root = short_root();
    let (client, server) = client_of_mock(root.path(), |hello| Hello {
        proto_minor: AGENT_ENV_PROTO_MINOR,
        capabilities: vec![Capability::Terminals, Capability::AgentEnvV1],
        ..hello
    });
    assert!(client.supports_agent_env());
    assert_eq!(
        client
            .spawn(with_env(), registration("env-new", None, false))
            .map_err(|f| f.message),
        Ok(Some(7))
    );
    drop(client);
    assert!(matches!(
        &server.join().unwrap()[..],
        [Request::Spawn { intent, .. }] if intent.agent_env.len() == 1
    ));
}

/// A terminal server from before the account-home request is never sent it:
/// the read is refused as an outdated server, which the UI offers to update.
#[test]
fn an_account_home_read_on_an_older_terminal_server_is_refused_as_outdated() {
    let root = short_root();
    let mock = mock_sidecar(root.path(), 1, |_, _, _| {});
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    assert_eq!(client.account_home(), Err(AccountHomeError::OutdatedServer));
    drop(client);
    assert!(mock.join().unwrap().is_empty(), "nothing was sent");
}

/// A terminal server from before the workspace head request is never sent it:
/// bringing work home is refused with a request to update the server.
#[test]
fn a_workspace_head_read_on_an_older_terminal_server_is_refused_before_sending() {
    let root = short_root();
    let mock = mock_sidecar(root.path(), 1, |_, _, _| {});
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let refused = client.workspace_head(codesense_pty_core::remote_files::FileTarget {
        owner_id: 41,
        origin_key: "a".repeat(64),
        request_id: uuid::Uuid::new_v4().to_string(),
        handle: format!("remote-workspace-{}", "b".repeat(64)),
    });
    assert_eq!(refused, Err(super::WorkspaceHeadError::OutdatedServer));
    assert!(refused
        .unwrap_err()
        .to_string()
        .contains("update the terminal server"));
    drop(client);
    assert!(mock.join().unwrap().is_empty(), "nothing was sent");
}

/// The built sidecar binary; `scripts/build-session-server.sh` builds it.
fn sidecar_binary() -> PathBuf {
    let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(format!("{SIDECAR_BIN}-{}", env!("TARGET_TRIPLE")));
    assert!(
        binary.is_file(),
        "build it first with scripts/build-session-server.sh: {binary:?}"
    );
    binary
}

/// The app's real launch path: the built sidecar binary, started through
/// `launch_detached`, serves this instance detached — its own session, not
/// the app's process group, already parented by launchd — and a relaunched app
/// rediscovers the same process.
#[test]
fn the_app_launches_a_detached_sidecar_it_can_rediscover() {
    let binary = sidecar_binary();
    let root = short_root();
    // Before the launch: reaps the sidecar even if connecting to it fails.
    let _reap = ReapSidecarsAt::new(root.path());
    let sink = || -> Arc<dyn TerminalEventSink> { Arc::new(RecordingSink::default()) };
    let state = SessionServerState::default();
    let client = state
        .connect_or_launch(root.path(), sink(), |root| {
            launch_detached_with_env(&binary, root, None, None, &[tie_to_this_process()])
        })
        .expect("the launched sidecar connects");
    let sidecar = client.pid();
    let pid = sidecar as libc::pid_t;
    let session = unsafe { libc::getsid(pid) };
    assert_ne!(session, unsafe { libc::getsid(0) }, "not the app's session");
    assert_ne!(session, pid, "not a session leader (double fork)");
    assert_ne!(
        unsafe { libc::getpgid(pid) },
        unsafe { libc::getpgrp() },
        "outside the app's process group"
    );
    #[cfg(target_os = "macos")]
    {
        let ppid = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &sidecar.to_string()])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap();
        assert_eq!(ppid, "1", "parented by launchd while the app still runs");
    }

    // The app quits and relaunches: the new process finds the same sidecar.
    drop(client);
    drop(state);
    let relaunched = SessionServerState::default();
    let again = relaunched
        .connect_or_launch(root.path(), sink(), no_launch)
        .unwrap();
    assert_eq!(again.pid(), sidecar);
}

/// The reaper a sidecar-launching test creates before its launch kills the
/// sidecar serving its root when dropped — even though the test never learnt
/// the sidecar's pid — and spares a sidecar serving any other root.
#[test]
fn a_launching_tests_reaper_kills_only_its_own_roots_sidecar() {
    let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(format!("{SIDECAR_BIN}-{}", env!("TARGET_TRIPLE")));
    assert!(
        binary.is_file(),
        "build it first with scripts/build-session-server.sh: {binary:?}"
    );
    let mine = short_root();
    let other = short_root();
    let _reap_other = ReapSidecarsAt::new(other.path());
    let reap_mine = ReapSidecarsAt::new(mine.path());
    for root in [mine.path(), other.path()] {
        launch_detached_with_env(&binary, root, None, None, &[tie_to_this_process()]).unwrap();
    }
    assert!(
        wait_until(|| !sidecar_pids_rooted_at(mine.path()).is_empty()
            && !sidecar_pids_rooted_at(other.path()).is_empty()),
        "both sidecars run"
    );
    let survivor = sidecar_pids_rooted_at(other.path());

    drop(reap_mine);
    assert!(
        wait_until(|| sidecar_pids_rooted_at(mine.path()).is_empty()),
        "the reaper kills the sidecar serving its root"
    );
    assert!(
        survivor.iter().all(|pid| alive(*pid)),
        "a sidecar serving another root is spared"
    );
}

/// A lost connection does not strand the sidecar's sessions: the next verb
/// reconnects to the sidecar that still listens, without launching one, and
/// events flow on the new connection; an instance that never used a sidecar
/// gets none.
#[test]
fn a_lost_connection_is_reestablished_without_launching() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let events = Arc::new(RecordingSink::default());
    let state = SessionServerState::default();
    let sink = || -> Arc<dyn TerminalEventSink> { events.clone() };
    assert!(
        state.reconnected(root.path()).is_none(),
        "never connected: no sidecar"
    );
    let first = state
        .connect_or_launch(root.path(), sink(), no_launch)
        .unwrap();
    spawn(
        &sidecar,
        &first,
        "sleep 60; :",
        registration("kept", None, false),
    );
    assert!(
        wait_until(|| first.is_idle(&sid("kept")) == Ok(false)),
        "the command is running before its connection is cut"
    );
    first.sever();
    assert!(wait_until(|| !first.is_alive()));

    let second = state.reconnected(root.path()).expect("reconnected");
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(second.is_alive());
    assert_eq!(
        second.is_idle(&sid("kept")),
        Ok(false),
        "the session is reachable again"
    );
    second.kill(&sid("kept")).unwrap();
    assert!(
        wait_until(|| events.events().iter().any(|event| matches!(
            event,
            TerminalDomainEvent::SessionRemoved { session, .. } if session.as_str() == "kept"
        ))),
        "events flow on the new connection"
    );
}

/// A failed reconnect is not retried within the backoff window, so a wedged
/// sidecar costs one handshake timeout, not one per request; once it listens
/// again, the next attempt after the window connects.
#[test]
fn a_failed_reconnect_backs_off() {
    let root = short_root();
    let _sidecar = TestServer::start_at(root.path());
    let state = SessionServerState::default();
    let sink = || -> Arc<dyn TerminalEventSink> { Arc::new(RecordingSink::default()) };
    let first = state
        .connect_or_launch(root.path(), sink(), no_launch)
        .unwrap();
    // The sidecar becomes unreachable: its socket is gone, the connection lost.
    let socket = socket_path(root.path());
    let away = root.path().join("away.sock");
    std::fs::rename(&socket, &away).unwrap();
    first.sever();
    assert!(wait_until(|| !first.is_alive()));

    assert!(state.reconnected(root.path()).is_none());
    assert!(lock(&state.attempts).last_reconnect_failure.is_some());
    std::fs::rename(&away, &socket).unwrap();
    assert!(
        state.reconnected(root.path()).is_none(),
        "inside the window: no second attempt, though the sidecar listens again"
    );
    lock(&state.attempts).last_reconnect_failure =
        Some(Instant::now() - RECONNECT_BACKOFF - Duration::from_millis(1));
    assert!(
        state.reconnected(root.path()).is_some(),
        "after the window it reconnects"
    );
}

/// Events for a withdrawn session — a lost spawn cancelled in the sidecar,
/// whose id may now be open in the app — are never delivered; others are.
#[test]
fn events_of_a_withdrawn_session_are_not_delivered() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let output = |id: &str| TerminalDomainEvent::Output {
        session: codesense_pty_core::domain::PtySessionId::new(id),
        data: "x".into(),
        seq: 1,
    };
    let mut egress = Egress::default();
    for event in [output("withdrawn"), output("kept")] {
        push(&theirs, &mut egress, event);
    }
    drop(theirs);
    let ledger = Ledger::default();
    lock(&ledger.withdrawn).insert("withdrawn".to_string());
    let events = Arc::new(RecordingSink::default());
    let namespace = EventNamespace {
        server_id: "local-test".into(),
        epoch: "epoch".into(),
        generation: 1,
    };
    let connection = Connection {
        pending: &Mutex::new(HashMap::new()),
        alive: &AtomicBool::new(true),
        rev: &AtomicU64::new(0),
        ledger: &ledger,
        client: &Weak::new(),
        namespace: &namespace,
        is_remote: false,
    };
    pump(
        BufReader::new(Box::new(ours)),
        connection,
        &PumpEventSink::Legacy(events.clone()),
        None,
    );
    assert_eq!(events.output("withdrawn"), "");
    assert_eq!(events.output("kept"), "x");
}

#[test]
fn rm4e_remote_worktree_live_is_dropped_before_the_sink() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let mut egress = Egress::default();
    push(
        &theirs,
        &mut egress,
        TerminalDomainEvent::WorktreeLive {
            cwd: PathBuf::from("/remote/worktree"),
        },
    );
    push(
        &theirs,
        &mut egress,
        TerminalDomainEvent::Output {
            session: PtySessionId::new("kept"),
            data: "ok".into(),
            seq: 2,
        },
    );
    drop(theirs);
    let ledger = Ledger::default();
    let events = Arc::new(RecordingSink::default());
    let namespace = EventNamespace {
        server_id: "verified-server".into(),
        epoch: "verified-epoch".into(),
        generation: 9,
    };
    let connection = Connection {
        pending: &Mutex::new(HashMap::new()),
        alive: &AtomicBool::new(true),
        rev: &AtomicU64::new(0),
        ledger: &ledger,
        client: &Weak::new(),
        namespace: &namespace,
        is_remote: true,
    };
    pump(
        BufReader::new(Box::new(ours)),
        connection,
        &PumpEventSink::Legacy(events.clone()),
        None,
    );
    assert_eq!(events.output("kept"), "ok");
    assert_eq!(events.events().len(), 1);
}

#[test]
fn rm4e_local_uds_reconnect_mints_a_fresh_connection_generation() {
    let root = short_root();
    let _sidecar = TestServer::start_at(root.path());
    let first = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let first_namespace = first.namespace().clone();
    drop(first);
    let second = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    assert_eq!(first_namespace.server_id, second.namespace().server_id);
    assert_eq!(first_namespace.epoch, second.namespace().epoch);
    assert_ne!(
        first_namespace.generation(),
        second.namespace().generation()
    );
}

#[test]
fn rm4e_connection_generation_exhaustion_fails_closed_without_reuse() {
    let counter = AtomicU64::new(u64::MAX - 1);
    assert_eq!(mint_connection_generation(&counter).unwrap(), u64::MAX);
    let error = mint_connection_generation(&counter).unwrap_err();
    assert!(error.to_string().contains("generation space exhausted"));
    assert_eq!(counter.load(Ordering::SeqCst), u64::MAX);
}

#[derive(Default)]
struct NamespacedRecordingSink(Mutex<Vec<NamespacedTerminalEvent>>);

impl NamespacedTerminalEventSink for NamespacedRecordingSink {
    fn emit(&self, envelope: NamespacedTerminalEvent) -> Result<(), PortUnavailable> {
        lock(&self.0).push(envelope);
        Ok(())
    }
}

#[test]
fn rm4e_retired_generation_cannot_publish_buffered_or_late_events() {
    let sink = Arc::new(NamespacedRecordingSink::default());
    let gate = RemotePublicationGate::new(sink.clone(), None);
    let namespace = EventNamespace {
        server_id: "verified-server".into(),
        epoch: "verified-epoch".into(),
        generation: 88,
    };
    NamespacedTerminalEventSink::emit(
        &*gate,
        NamespacedTerminalEvent {
            namespace: namespace.clone(),
            event: TerminalDomainEvent::Output {
                session: PtySessionId::new("same"),
                data: "buffered".into(),
                seq: 8,
            },
        },
    )
    .unwrap();
    gate.suppress();
    gate.publish();
    NamespacedTerminalEventSink::emit(
        &*gate,
        NamespacedTerminalEvent {
            namespace,
            event: TerminalDomainEvent::Output {
                session: PtySessionId::new("same"),
                data: "late".into(),
                seq: 12,
            },
        },
    )
    .unwrap();
    assert!(lock(&sink.0).is_empty());
}

#[test]
fn rm4e_remote_projected_family_is_stamped_once_before_effects() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let mut egress = Egress::default();
    let session = PtySessionId::new("collision");
    let events = [
        TerminalDomainEvent::Output {
            session: session.clone(),
            data: "output".into(),
            seq: 6,
        },
        TerminalDomainEvent::StreamOutput {
            stream: StreamId(7),
            data: "stream".into(),
            seq: 6,
        },
        TerminalDomainEvent::PromptMarker {
            session: session.clone(),
            marker: OscEvent::CommandEnd(Some(0)),
        },
        TerminalDomainEvent::Exit {
            session: session.clone(),
            status: Some(ExitInfo {
                exit_code: Some(0),
                signal: None,
            }),
            pool_member: false,
            requested: false,
            incarnation: 0,
        },
        TerminalDomainEvent::SessionRemoved {
            session,
            reason: TeardownReason::Kill,
        },
        TerminalDomainEvent::LayoutChanged {
            epoch: "verified-epoch".into(),
            rev: 4,
            workspace: WorkspaceKey::new("same-workspace"),
            layout: None,
        },
    ];
    for event in events {
        push(&theirs, &mut egress, event);
    }
    drop(theirs);

    let ledger = Ledger::default();
    let sink = Arc::new(NamespacedRecordingSink::default());
    let namespace = EventNamespace {
        server_id: "verified-server".into(),
        epoch: "verified-epoch".into(),
        generation: 91,
    };
    let revision = AtomicU64::new(0);
    let connection = Connection {
        pending: &Mutex::new(HashMap::new()),
        alive: &AtomicBool::new(true),
        rev: &revision,
        ledger: &ledger,
        client: &Weak::new(),
        namespace: &namespace,
        is_remote: true,
    };
    pump(
        BufReader::new(Box::new(ours)),
        connection,
        &PumpEventSink::Namespaced(sink.clone()),
        None,
    );

    let captured = lock(&sink.0);
    assert_eq!(captured.len(), 6);
    assert!(captured.iter().all(|event| event.namespace == namespace));
    assert!(matches!(
        captured[0].event,
        TerminalDomainEvent::Output { .. }
    ));
    assert!(matches!(
        captured[1].event,
        TerminalDomainEvent::StreamOutput { .. }
    ));
    assert!(matches!(
        captured[2].event,
        TerminalDomainEvent::PromptMarker { .. }
    ));
    assert!(matches!(
        captured[3].event,
        TerminalDomainEvent::Exit { .. }
    ));
    assert!(matches!(
        captured[4].event,
        TerminalDomainEvent::SessionRemoved { .. }
    ));
    assert!(matches!(
        captured[5].event,
        TerminalDomainEvent::LayoutChanged { .. }
    ));
    assert_eq!(revision.load(Ordering::SeqCst), 4);
}

#[test]
fn rm4e_layout_epoch_mismatch_is_rejected_before_revision_or_publication() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let mut egress = Egress::default();
    push(
        &theirs,
        &mut egress,
        TerminalDomainEvent::LayoutChanged {
            epoch: "forged-epoch".into(),
            rev: 99,
            workspace: WorkspaceKey::new("same-workspace"),
            layout: None,
        },
    );
    drop(theirs);
    let ledger = Ledger::default();
    let sink = Arc::new(NamespacedRecordingSink::default());
    let namespace = EventNamespace {
        server_id: "verified-server".into(),
        epoch: "verified-epoch".into(),
        generation: 92,
    };
    let revision = AtomicU64::new(3);
    let connection = Connection {
        pending: &Mutex::new(HashMap::new()),
        alive: &AtomicBool::new(true),
        rev: &revision,
        ledger: &ledger,
        client: &Weak::new(),
        namespace: &namespace,
        is_remote: true,
    };
    pump(
        BufReader::new(Box::new(ours)),
        connection,
        &PumpEventSink::Namespaced(sink.clone()),
        None,
    );
    assert!(lock(&sink.0).is_empty());
    assert_eq!(revision.load(Ordering::SeqCst), 3);
}

/// A lost spawn whose cancel is confirmed is withdrawn from the sidecar's event
/// stream before the cancel is sent, so the teardown the cancel causes never
/// reaches the app; the withdrawal outlives a reconnect, and a later sidecar
/// spawn of the id makes its events live again from the first.
#[test]
fn a_cancelled_lost_spawn_stays_withdrawn_across_reconnects_until_respawned() {
    let root = short_root();
    let events = Arc::new(RecordingSink::default());
    let removed = |id: &'static str| TerminalDomainEvent::SessionRemoved {
        session: codesense_pty_core::domain::PtySessionId::new(id),
        reason: TeardownReason::Kill,
    };
    // Connection 0 loses the spawn; the cancel connection confirms and, like a
    // sidecar tearing the session down, pushes its removal on connection 2.
    let pushed = Arc::new(Mutex::new(None::<UnixStream>));
    let events_conn = pushed.clone();
    let mock = mock_sidecar(root.path(), 3, move |n, stream, rid| match n {
        0 => stream.shutdown(std::net::Shutdown::Both).unwrap(),
        1 => answer(stream, rid, Ok(Reply::Done)),
        _ => {
            *lock(&events_conn) = Some(stream.try_clone().unwrap());
            answer(stream, rid, Ok(Reply::Flag { value: true }));
        }
    });
    let state = SessionServerState::default();
    let sink = || -> Arc<dyn TerminalEventSink> { events.clone() };
    let client = state
        .connect_or_launch(root.path(), sink(), no_launch)
        .unwrap();
    let failure = client
        .spawn(sh("true"), registration("gone", None, false))
        .expect_err("lost");
    assert!(failure.may_host_elsewhere);

    // Reconnect (the first connection is dead) and push stale and live events.
    let again = state.reconnected(root.path()).expect("reconnected");
    assert_eq!(again.is_idle(&sid("probe")), Ok(true));
    let stream = lock(&pushed).take().expect("events connection");
    let mut egress = Egress::default();
    for event in [removed("gone"), removed("other")] {
        push(&stream, &mut egress, event);
    }
    assert!(wait_until(|| {
        events
        .events()
        .iter()
        .any(|e| matches!(e, TerminalDomainEvent::SessionRemoved { session, .. } if session.as_str() == "other"))
    }));
    assert!(
        !events.events().iter().any(
            |e| matches!(e, TerminalDomainEvent::SessionRemoved { session, .. } if session.as_str() == "gone")
        ),
        "the withdrawn id's teardown is not delivered, even on a new connection"
    );
    drop(stream);
    mock.join().unwrap();
}

/// A colliding in-process wire ID must not suppress the sidecar member's events.
#[test]
fn rm5aa_local_id_collision_does_not_withdraw_peer_events() {
    let state = SessionServerState::default();
    state.withdraw("before");
    assert!(
        lock(&state.ledger.withdrawn).is_empty(),
        "sidecar mode off: nothing to withdraw"
    );

    let unreachable = short_root();
    assert!(state
        .connect_or_launch(
            unreachable.path(),
            Arc::new(RecordingSink::default()),
            no_launch
        )
        .is_err());
    state.withdraw("unreachable");
    assert!(
        lock(&state.ledger.withdrawn).is_empty(),
        "an in-process ID is not authority to withdraw a sidecar ID"
    );
}

/// A warm handoff into a withdrawn id hosts it in the sidecar again, so its
/// events are delivered from the first.
#[test]
fn a_warm_handoff_makes_its_target_live_again() {
    let root = short_root();
    let mock = mock_sidecar(root.path(), 1, |_, stream, rid| {
        answer(stream, rid, Ok(Reply::Done));
        let event = TerminalDomainEvent::Output {
            session: codesense_pty_core::domain::PtySessionId::new("back"),
            data: "hi".into(),
            seq: 2,
        };
        push(stream, &mut Egress::default(), event);
    });
    let events = Arc::new(RecordingSink::default());
    let state = SessionServerState::default();
    let client = state
        .connect_or_launch(root.path(), events.clone(), no_launch)
        .unwrap();
    lock(&state.ledger.withdrawn).insert("back".into());
    assert!(
        lock(&state.ledger.withdrawn).contains("back"),
        "withdrawn before the handoff"
    );
    client
        .handoff_warm_member(
            &sid("__warm_pool_1"),
            &sid("back"),
            "/tmp",
            80,
            24,
            SessionMeta::default(),
        )
        .unwrap();
    assert!(wait_until(|| events.output("back") == "hi"));
    mock.join().unwrap();
}

/// The app strips exactly the variable the sidecar reads as its lifetime tie.
#[test]
fn the_stripped_tie_variable_is_the_one_the_sidecar_reads() {
    assert_eq!(
        SIDECAR_EXIT_WITH_PID_ENV,
        codesense_session_server::exit_with::EXIT_WITH_PID_ENV
    );
}

/// A restart never ends a compatible sidecar (one an earlier restart already
/// started): it only connects to it. Ending a real sidecar ends its sessions;
/// the one launched after it is a new lifetime, and the connection after it
/// reads as a restart. Only a terminal server is ever ended.
#[test]
fn a_restart_leaves_a_compatible_sidecar_and_ending_one_ends_its_sessions() {
    let binary = sidecar_binary();
    let root = short_root();
    // Before the launch: reaps every sidecar at this root however the test ends.
    let _reap = ReapSidecarsAt::new(root.path());
    let sink = || -> Arc<dyn TerminalEventSink> { Arc::new(RecordingSink::default()) };
    let launch =
        |root: &Path| launch_detached_with_env(&binary, root, None, None, &[tie_to_this_process()]);
    let state = SessionServerState::default();
    let before = state
        .connect_or_launch(root.path(), sink(), launch)
        .unwrap();
    let child = before
        .spawn(sh("sleep 60"), registration("doomed", None, false))
        .unwrap()
        .unwrap();

    let (same, ended) = state
        .restart(root.path(), sink(), |_| panic!("never ended"), launch)
        .expect("connected");
    assert_eq!(ended, None);
    assert_eq!(same.pid(), before.pid());
    assert!(alive(child), "its session carries on");

    terminate_sidecar(before.pid()).expect("ended");
    assert!(!alive(before.pid()), "the old sidecar ended");
    assert!(wait_until(|| !alive(child)), "its session ended with it");
    assert!(wait_until(|| !before.is_alive()));
    let after = state
        .connect_or_launch(root.path(), sink(), launch)
        .expect("relaunched");
    assert_ne!(after.pid(), before.pid());
    assert_ne!(after.epoch(), before.epoch());
    assert_eq!(
        state.take_change(),
        Some(Continuity::Restarted {
            ended: vec![(sid("doomed"), false)]
        })
    );
    assert_eq!(after.has_session(&sid("doomed")), Ok(false));

    let not_a_server = terminate_sidecar(std::process::id()).unwrap_err();
    assert!(not_a_server.contains("is not a"), "{not_a_server}");
}

/// A protocol-1 UDS sidecar receives no compatibility probe. Only the pinned
/// remote SSH control plane is allowed to classify protocol 1.
#[test]
fn rm4c_uds_protocol_1_receives_zero_client_bytes() {
    use std::io::Read;
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let host = std::thread::spawn(move || {
        let (silent, _) = listener.accept().unwrap();
        let mut ignored = Vec::new();
        let _ = (&silent).read_to_end(&mut ignored);
        ignored
    });
    let refused = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    );
    assert!(matches!(refused, Err(ConnectError::Refused(_))));
    assert!(host.join().unwrap().is_empty());
}

/// A mock sidecar living through `lifetimes`: each connection is greeted as
/// the `(epoch, rev)` given, its listen answered and `pushes` sent, and it is
/// held until the client closes it.
fn mock_lifetimes(
    root: &Path,
    lifetimes: Vec<(&'static str, u64, Vec<TerminalDomainEvent>)>,
) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(socket_path(root)).unwrap();
    let hello = mock_hello(instance_id(root));
    std::thread::spawn(move || {
        for (epoch, rev, pushes) in lifetimes {
            let (stream, _) = listener.accept().unwrap();
            let greeting = Hello {
                epoch: epoch.to_string(),
                rev,
                ..hello.clone()
            };
            greet_client(&stream, &greeting, 0);
            let mut egress = Egress::default();
            while let Some((rid, request)) = next_call(&stream) {
                if matches!(request, Request::Listen) {
                    answer(&stream, rid, Ok(Reply::Done));
                    for event in pushes.clone() {
                        push(&stream, &mut egress, event);
                    }
                }
            }
        }
    })
}

/// A reconnect tells the sidecar lifetime it reaches: the same epoch resumed
/// (with whether its layout moved past the newest revision the connection
/// before saw, pushes included), or a new epoch — a restart that ended the
/// sessions. The app's first connection is neither.
#[test]
fn a_reconnect_tells_a_resumed_sidecar_from_a_restarted_one() {
    let root = short_root();
    let change = |rev: u64| TerminalDomainEvent::LayoutChanged {
        epoch: "e1".into(),
        rev,
        workspace: WorkspaceKey::new("ws"),
        layout: None,
    };
    let mock = mock_lifetimes(
        root.path(),
        vec![
            ("e1", 3, Vec::new()),
            ("e1", 3, vec![change(5)]),
            ("e1", 5, Vec::new()),
            ("e1", 6, Vec::new()),
            ("e2", 0, Vec::new()),
        ],
    );
    let state = SessionServerState::default();
    let close = |client: Arc<SessionServerClient>| {
        client.sever();
        assert!(wait_until(|| !client.is_alive()));
    };
    let first = state
        .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    assert_eq!(state.take_change(), None, "the first connection");
    close(first);
    let next = |expected: Continuity| {
        let client = state.reconnected(root.path()).expect("reconnected");
        assert_eq!(state.take_change(), Some(expected));
        client
    };
    let second = next(Continuity::Resumed {
        layout_moved: false,
    });
    assert!(
        wait_until(|| second.rev() == 5),
        "a pushed change advances rev"
    );
    close(second);
    close(next(Continuity::Resumed {
        layout_moved: false,
    }));
    close(next(Continuity::Resumed { layout_moved: true }));
    let restarted = next(Continuity::Restarted { ended: Vec::new() });
    assert_eq!(restarted.epoch(), "e2");
    assert_eq!(state.take_change(), None, "taken once");
    close(restarted);
    mock.join().unwrap();
}

/// Changes not yet taken merge: a layout move seen on any resumed connection
/// stands, and a restart outranks a later resume. A restart takes every
/// session the app hosted in the old lifetime — except one the app now hosts
/// itself — at the moment it is adopted.
#[test]
fn untaken_changes_merge_and_a_restart_takes_the_old_lifetimes_sessions() {
    let root = short_root();
    let mock = mock_lifetimes(
        root.path(),
        vec![
            ("e1", 3, Vec::new()),
            ("e1", 4, Vec::new()),
            ("e1", 4, Vec::new()),
            ("e2", 0, Vec::new()),
            ("e2", 0, Vec::new()),
        ],
    );
    let state = SessionServerState::default();
    let close = |client: Arc<SessionServerClient>| {
        client.sever();
        assert!(wait_until(|| !client.is_alive()));
    };
    let reconnect = || state.reconnected(root.path()).expect("reconnected");
    close(
        state
            .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
            .unwrap(),
    );
    close(reconnect());
    close(reconnect());
    assert_eq!(
        state.take_change(),
        Some(Continuity::Resumed { layout_moved: true }),
        "the move seen by the first resume stands"
    );

    lock(&state.ledger.hosted).insert("pooled".into(), (true, 0));
    lock(&state.ledger.hosted).insert("moved-in".into(), (false, 0));
    lock(&state.ledger.withdrawn).insert("moved-in".into());
    close(reconnect());
    lock(&state.ledger.hosted).insert("new-lifetime".into(), (false, 0));
    close(reconnect());
    assert_eq!(
        state.take_change(),
        Some(Continuity::Restarted {
            ended: vec![(sid("pooled"), true)]
        }),
        "a later resume keeps the restart; the new lifetime's session is not ended"
    );
    assert!(lock(&state.ledger.hosted).contains_key("new-lifetime"));
    mock.join().unwrap();
}

/// A spawn awaiting its reply is never read as ended: it is left to its
/// reply. A failed respawn of a hosted id leaves the running one hosted.
#[test]
fn a_spawn_in_flight_or_failed_keeps_the_ledger_true() {
    let state = SessionServerState::default();
    lock(&state.ledger.hosted).insert("running".into(), (false, 0));
    lock(&state.ledger.hosted).insert("opening".into(), (false, 0));
    lock(&state.ledger.spawning).insert("opening".into());
    assert_eq!(state.take_ended(|_| true), [(sid("running"), false)]);
    assert!(lock(&state.ledger.hosted).contains_key("opening"));

    let root = short_root();
    let mock = mock_sidecar(root.path(), 1, |_, stream, rid| {
        answer(stream, rid, Err("spawn_pty: refused".into()));
    });
    let client = state
        .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    lock(&state.ledger.hosted).insert("kept".into(), (false, 0));
    assert!(client
        .spawn(sh("true"), registration("kept", None, true))
        .is_err());
    assert_eq!(lock(&state.ledger.hosted).get("kept"), Some(&(false, 0)));
    assert!(!lock(&state.ledger.spawning).contains("kept"));
    mock.join().unwrap();

    // A restart takes a spawn in flight with its lifetime.
    lock(&state.ledger.spawning).insert("opening".into());
    let taken = state.ledger.take_all();
    assert!(taken.contains(&(sid("opening"), false)) && taken.contains(&(sid("kept"), false)));
    assert!(lock(&state.ledger.hosted).is_empty());
}

/// A spawn whose lifetime ended while it awaited its reply leaves the ledger
/// to the restart that took it: a failure never restores the dead entry.
#[test]
fn a_spawn_outlived_by_its_lifetime_leaves_the_ledger_alone() {
    let root = short_root();
    let state = Arc::new(SessionServerState::default());
    let taken = Arc::new(Mutex::new(None));
    let (restarting, into) = (state.clone(), taken.clone());
    let mock = mock_sidecar(root.path(), 1, move |_, stream, rid| {
        // The lifetime ends while the spawn awaits its reply.
        *lock(&into) = Some(restarting.ledger.take_all());
        answer(stream, rid, Err("spawn_pty: refused".into()));
    });
    let client = state
        .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    lock(&state.ledger.hosted).insert("respawned".into(), (false, 0));
    assert!(client
        .spawn(sh("true"), registration("respawned", None, false))
        .is_err());
    mock.join().unwrap();
    assert_eq!(*lock(&taken), Some(vec![(sid("respawned"), false)]));
    assert!(
        lock(&state.ledger.hosted).is_empty(),
        "the dead lifetime's session is not restored"
    );
}

/// A resume never takes an entry recorded after it was checked: a respawn of
/// the id that began — and even finished — while the sidecar was asked stands.
#[test]
fn a_resume_takes_only_the_entry_it_checked() {
    let state = SessionServerState::default();
    lock(&state.ledger.hosted).insert("respawned".into(), (false, 0));
    lock(&state.ledger.hosted).insert("gone".into(), (false, 0));
    let taken = state.take_ended(|session| {
        // A respawn of the id is recorded while the sidecar is asked.
        let mut hosted = lock(&state.ledger.hosted);
        state.ledger.record(&mut hosted, "respawned", false);
        session.as_str() != "missing"
    });
    assert_eq!(taken, [(sid("gone"), false)]);
    assert!(lock(&state.ledger.hosted).contains_key("respawned"));
}

/// Every `Exit` `sink` recorded for `session`, by status.
fn exits_of(sink: &RecordingSink, session: &str) -> Vec<Option<ExitInfo>> {
    sink.events()
        .into_iter()
        .filter_map(|event| match event {
            TerminalDomainEvent::Exit {
                session: s, status, ..
            } if s.as_str() == session => Some(status),
            _ => None,
        })
        .collect()
}

fn removed_from(sink: &RecordingSink, session: &str) -> bool {
    sink.events().iter().any(|event| {
        matches!(event, TerminalDomainEvent::SessionRemoved { session: s, .. } if s.as_str() == session)
    })
}

fn exit_3() -> Option<ExitInfo> {
    Some(ExitInfo {
        exit_code: Some(3),
        signal: None,
    })
}

/// A session whose child exited while no connection could hear it stays a
/// member of the sidecar's registry (retained, its terminal readable), so
/// membership alone reads it as running. On a resume of the same lifetime it
/// still ends in the app, with its real status and without being removed; a
/// running session is left alone.
#[test]
fn a_resume_ends_a_retained_session_whose_exit_was_lost_with_its_status() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let sink = Arc::new(RecordingSink::default());
    let state = SessionServerState::default();
    let prior = state
        .connect_or_launch(root.path(), sink.clone(), no_launch)
        .unwrap();
    spawn(
        &sidecar,
        &prior,
        "cat",
        registration("running", None, false),
    );
    let pid = spawn(
        &sidecar,
        &prior,
        "sleep 1; exit 3",
        registration("exited", None, false),
    );
    prior.sever();
    assert!(wait_until(|| !prior.is_alive()));
    assert!(wait_until(|| !alive(pid)), "it exits while no one listens");

    let client = state.reconnected(root.path()).expect("reconnected");
    assert!(matches!(
        state.take_change(),
        Some(Continuity::Resumed { .. })
    ));
    assert_eq!(client.has_session(&sid("exited")), Ok(true), "retained");
    assert!(exits_of(&sink, "exited").is_empty(), "its exit was lost");
    crate::commands::pty_backend::end_missed_sessions(&state, &client, &*sink);
    assert!(
        wait_until(|| !exits_of(&sink, "exited").is_empty()),
        "the lost exit reaches the app on the resume"
    );
    assert_eq!(exits_of(&sink, "exited"), [exit_3()]);
    assert!(
        !removed_from(&sink, "exited"),
        "it stays readable in the sidecar"
    );
    assert!(exits_of(&sink, "running").is_empty());
    assert!(!removed_from(&sink, "running"));
    assert!(state.unexited_sessions().contains(&sid("running")));
    assert!(!state.unexited_sessions().contains(&sid("exited")));
    assert!(client
        .alive_session_ids(&[sid("running")])
        .unwrap()
        .contains(&sid("running")));
    client.kill(&sid("running")).unwrap();
    client.kill(&sid("exited")).unwrap();
}

/// The sidecar repeats a retained session's exit on every attach, and on a
/// resume for a session whose exit the app has not had. An app that has it
/// drops the repeat, across reconnects; an app that never had it (a fresh
/// launch, another app) gets it on its attach. A respawn of the id is news
/// again: its own exit is delivered.
#[test]
fn an_exit_reaches_each_app_once() {
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let sink = Arc::new(RecordingSink::default());
    let state = SessionServerState::default();
    let prior = state
        .connect_or_launch(root.path(), sink.clone(), no_launch)
        .unwrap();
    spawn(
        &sidecar,
        &prior,
        "exit 3",
        registration("ended", None, false),
    );
    assert!(wait_until(|| !exits_of(&sink, "ended").is_empty()));
    let viewport = ViewportId::new("pane");
    let attached = prior.subscribe(&sid("ended"), &viewport).unwrap();
    prior.unsubscribe(attached.stream_id).unwrap();

    prior.sever();
    assert!(wait_until(|| !prior.is_alive()));
    let client = state.reconnected(root.path()).expect("reconnected");
    assert!(matches!(
        state.take_change(),
        Some(Continuity::Resumed { .. })
    ));
    crate::commands::pty_backend::end_missed_sessions(&state, &client, &*sink);
    let attached = client.subscribe(&sid("ended"), &viewport).unwrap();
    client.unsubscribe(attached.stream_id).unwrap();

    // A fresh app on the same sidecar never heard the exit: its attach does.
    let fresh = Arc::new(RecordingSink::default());
    let other =
        SessionServerClient::connect(root.path(), fresh.clone(), Arc::default(), None).unwrap();
    let viewed = other
        .subscribe(&sid("ended"), &ViewportId::new("other"))
        .unwrap();
    assert!(wait_until(|| !exits_of(&fresh, "ended").is_empty()));
    assert_eq!(exits_of(&fresh, "ended"), [exit_3()]);
    other.unsubscribe(viewed.stream_id).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        exits_of(&sink, "ended"),
        [exit_3()],
        "the first app drops every repeat"
    );

    // The id's next process: alive, then its own exit is delivered.
    spawn(
        &sidecar,
        &client,
        "read line; exit 7",
        registration("ended", None, false),
    );
    client.write(&sid("ended"), b"go\n").unwrap();
    assert!(wait_until(|| exits_of(&sink, "ended").len() == 2));
    assert_eq!(
        exits_of(&sink, "ended")[1],
        Some(ExitInfo {
            exit_code: Some(7),
            signal: None,
        })
    );
    client.kill(&sid("ended")).unwrap();
}

fn exit_of(session: &str, incarnation: u64) -> TerminalDomainEvent {
    TerminalDomainEvent::Exit {
        session: sid(session),
        status: exit_3(),
        pool_member: false,
        requested: false,
        incarnation,
    }
}

/// A repeat of an old process's exit that arrives after this app opened the
/// id again (a displacing spawn: no removal in between) is still a repeat:
/// dropped, never read as the new process's exit, which is delivered when it
/// comes, once. Only the very process's repeat is dropped: an exit of an
/// older incarnation never delivered (a handed-off pool shell) is delivered,
/// and a superseded lifetime's exit never blocks the new lifetime's. Only an
/// exit the app never had is asked for on a resume.
#[test]
fn a_late_repeat_of_an_old_process_exit_is_dropped_after_a_respawn() {
    let ledger = Ledger::default();
    let record = |session: &str| {
        let mut hosted = lock(&ledger.hosted);
        ledger.record(&mut hosted, session, false);
    };
    record("tab");
    assert!(ledger.deliver_once("e1", &exit_of("tab", 7)));
    assert!(!ledger.deliver_once("e1", &exit_of("tab", 7)), "a repeat");
    assert!(ledger.unexited_sessions().is_empty(), "its exit is had");

    record("tab");
    assert_eq!(ledger.unexited_sessions(), [sid("tab")], "a new process");
    assert!(
        !ledger.deliver_once("e1", &exit_of("tab", 7)),
        "the old process's repeat, arriving after the respawn"
    );
    assert_eq!(ledger.unexited_sessions(), [sid("tab")]);
    assert!(
        ledger.deliver_once("e1", &exit_of("tab", 9)),
        "the new process's exit"
    );
    assert!(!ledger.deliver_once("e1", &exit_of("tab", 9)));
    assert!(
        !ledger.deliver_once("e1", &exit_of("tab", 7)),
        "an older process's repeat, arriving after the newer exit"
    );
    assert!(
        ledger.deliver_once("e1", &exit_of("tab", 3)),
        "a handed-off pool shell's exit: older, but never delivered"
    );

    // A superseded lifetime's exit never blocks the new lifetime's process
    // of the same incarnation.
    assert!(ledger.deliver_once("e0", &exit_of("other", 5)));
    assert!(ledger.deliver_once("e1", &exit_of("other", 5)));

    // A removal ends the record; an exit with no incarnation (an older
    // host, which never repeats one) is always delivered.
    assert!(ledger.deliver_once(
        "e1",
        &TerminalDomainEvent::SessionRemoved {
            session: sid("tab"),
            reason: TeardownReason::Kill,
        }
    ));
    assert!(ledger.deliver_once("e1", &exit_of("tab", 9)));
    assert!(ledger.deliver_once("e1", &exit_of("old-host", 0)));
    assert!(ledger.deliver_once("e1", &exit_of("old-host", 0)));
}

/// A reconnect that finds this instance's sidecar incompatible records it as
/// a launch-or-connect does (for the restart prompt), and a later reconnect
/// to a compatible one clears it. One that finds the socket served by another
/// instance's sidecar records nothing that would stop a later try.
#[test]
fn a_reconnect_records_an_incompatible_sidecar_until_a_compatible_one_answers() {
    let sink: Arc<dyn TerminalEventSink> = Arc::new(RecordingSink::default());

    let root = short_root();
    let older = Hello {
        proto_major: PROTO_MAJOR - 2,
        ..mock_hello(instance_id(root.path()))
    };
    let mock = mock_host(root.path(), older, 0);
    let state = SessionServerState::default();
    *lock(&state.sink) = Some(sink.clone());
    assert!(state.reconnected(root.path()).is_none());
    assert!(mock.join().unwrap().is_err());
    assert!(state.incompatible().is_some());
    let _ = std::fs::remove_file(socket_path(root.path()));
    let _compatible = TestServer::start_at(root.path());
    lock(&state.attempts).last_reconnect_failure = None;
    assert!(state.reconnected(root.path()).is_some(), "it comes back");
    assert_eq!(state.incompatible(), None);

    let other = short_root();
    let _other_sidecar = TestServer::start_at(other.path());
    let foreign = short_root();
    std::os::unix::fs::symlink(socket_path(other.path()), socket_path(foreign.path())).unwrap();
    let state = SessionServerState::default();
    *lock(&state.sink) = Some(sink);
    assert!(state.reconnected(foreign.path()).is_none());
    assert_eq!(state.incompatible(), None);
}

/// SV-7: a claim the sidecar did not answer is tried again on the next call;
/// one it answered is not sent again (until asked anew).
#[test]
fn sv7_a_failed_claim_is_tried_again() {
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let hello = Hello {
        capabilities: vec![Capability::Terminals, Capability::ServerStatusV1],
        ..mock_hello(instance_id(root.path()))
    };
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        greet_client(&stream, &hello, 0);
        let mut claims = 0;
        while let Some((rid, request)) = next_call(&stream) {
            match request {
                Request::Listen => answer(&stream, rid, Ok(Reply::Done)),
                Request::ClaimSessions { sessions } => {
                    assert_eq!(sessions, [sid("background")]);
                    claims += 1;
                    let result = if claims == 1 {
                        Err("busy".into())
                    } else {
                        Ok(Reply::Done)
                    };
                    answer(&stream, rid, result);
                    if claims == 2 {
                        return claims;
                    }
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        claims
    });
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    assert!(client.claim_once(&[sid("background")], false).is_err());
    assert!(client.claim_once(&[sid("background")], false).is_ok());
    assert!(
        client.claim_once(&[sid("background")], false).is_ok(),
        "answered: not sent again"
    );
    assert_eq!(server.join().unwrap(), 2);
}

/// SV-7: a server stopped on purpose stays quiet only as that server: its
/// loss goes unannounced, and the stop is over once the app connects to
/// another server.
#[test]
fn sv7_a_stop_is_over_once_another_server_is_connected() {
    let root = short_root();
    let _server = TestServer::start_at(root.path());
    let state = SessionServerState::default();
    let client = state
        .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    state.mark_user_stopped(client.server_id(), client.epoch());
    client.sever();
    assert!(wait_until(|| !client.is_alive()));
    assert!(state.user_stopped());
    assert!(state.lost_server_was_stopped(), "its own loss is quiet");
    // The same server, reconnected: still the stopped one.
    let again = state.reconnected(root.path()).expect("reconnected");
    assert_eq!(again.epoch(), client.epoch());
    assert!(state.user_stopped());

    // A stop recorded for some other server: connecting to this one ends it.
    again.sever();
    assert!(wait_until(|| !again.is_alive()));
    state.mark_user_stopped("another-server", "another-epoch");
    let other = state.reconnected(root.path()).expect("reconnected");
    assert_ne!(
        (other.server_id(), other.epoch()),
        ("another-server", "another-epoch")
    );
    assert!(
        !state.user_stopped(),
        "connected to another server: the stop is over"
    );
    assert!(!state.lost_server_was_stopped());
}

/// A warm handoff moves the ledger's entry: the session it opened is hosted
/// (not as a pool member) and the member is not.
#[test]
fn a_warm_handoff_moves_the_ledger_entry() {
    let root = short_root();
    let mock = mock_sidecar(root.path(), 1, |_, stream, rid| {
        answer(stream, rid, Ok(Reply::Done));
    });
    let state = SessionServerState::default();
    let client = state
        .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    lock(&state.ledger.hosted).insert("__warm_pool_1".into(), (true, 0));
    client
        .handoff_warm_member(
            &sid("__warm_pool_1"),
            &sid("opened"),
            "/tmp",
            80,
            24,
            SessionMeta::default(),
        )
        .unwrap();
    assert_eq!(state.take_ended(|_| true), [(sid("opened"), false)]);
    mock.join().unwrap();
}

/// Restarting an incompatible sidecar ends the process its hello names and
/// reports how many sessions its census counted.
#[test]
fn a_restart_of_an_incompatible_sidecar_reports_its_census() {
    let root = short_root();
    let older = Hello {
        proto_major: PROTO_MAJOR - 2,
        pid: 4242,
        ..mock_hello(instance_id(root.path()))
    };
    let mock = mock_host(root.path(), older, 5);
    let state = SessionServerState::default();
    let ended_pid = std::cell::Cell::new(None);
    let launched = std::cell::RefCell::new(None);
    let (client, ended) = state
        .restart(
            root.path(),
            Arc::new(RecordingSink::default()),
            |pid| {
                ended_pid.set(Some(pid));
                Ok(())
            },
            |root| {
                *launched.borrow_mut() = Some(TestServer::start_at(root));
                Ok(())
            },
        )
        .expect("restarted");
    assert!(mock.join().unwrap().is_err());
    assert_eq!(ended_pid.get(), Some(4242));
    assert_eq!(ended, Some(5));
    assert!(client.is_alive());
    assert_eq!(state.incompatible(), None, "the new server is compatible");
}

/// A restart that ended the old sidecar but could not start a new one says
/// so; one that could not end it says the old one still runs.
#[test]
fn a_restart_failure_says_whether_the_old_sidecar_stopped() {
    let root = short_root();
    let older = Hello {
        proto_major: PROTO_MAJOR - 2,
        pid: 4242,
        ..mock_hello(instance_id(root.path()))
    };
    let mock = mock_host(root.path(), older.clone(), 2);
    let state = SessionServerState::default();
    let sink = || -> Arc<dyn TerminalEventSink> { Arc::new(RecordingSink::default()) };
    let failure = state
        .restart(root.path(), sink(), |_| Ok(()), |_| Err("no binary".into()))
        .err()
        .expect("the new sidecar did not start");
    assert!(mock.join().unwrap().is_err());
    assert!(failure.old_stopped, "{failure:?}");
    assert!(matches!(failure.error, ConnectError::Unreachable(_)));

    let root = short_root();
    let mock = mock_host(root.path(), older, 2);
    let failure = state
        .restart(
            root.path(),
            sink(),
            |_| Err("not a terminal server".into()),
            |_| panic!("never launched over a running one"),
        )
        .err()
        .expect("the old sidecar was not ended");
    assert!(mock.join().unwrap().is_err());
    assert!(!failure.old_stopped, "{failure:?}");
}

/// The incompatible status stands while the incompatible sidecar answers, and
/// clears once something else — or nothing — serves the socket.
#[test]
fn the_incompatible_status_clears_once_that_sidecar_is_gone() {
    let root = short_root();
    let older = Hello {
        proto_major: PROTO_MAJOR - 2,
        ..mock_hello(instance_id(root.path()))
    };
    let mock = mock_host(root.path(), older, 2);
    let state = SessionServerState::default();
    let sink = || -> Arc<dyn TerminalEventSink> { Arc::new(RecordingSink::default()) };
    assert!(state
        .connect_or_launch(root.path(), sink(), no_launch)
        .is_err());
    assert!(mock.join().unwrap().is_err());
    assert_eq!(
        state.incompatible().map(|skew| skew.live_sessions),
        Some(Some(2))
    );
    lock(&state.attempts).incompatible = state.incompatible().map(|skew| {
        (
            Instant::now() - RECONNECT_BACKOFF - Duration::from_millis(1),
            skew,
        )
    });
    assert!(state
        .connect_or_launch(root.path(), sink(), no_launch)
        .is_err());
    assert_eq!(
        state.incompatible(),
        None,
        "nothing incompatible serves the socket now"
    );
}

/// A server the kernel names by an `activity-helper` hard link is still ended
/// as a terminal server: the link in its own root, or another root's link to
/// the same file. A helper process, a wrapper, an argv that is not a server's,
/// or a helper of another file elsewhere is not.
#[test]
fn a_server_named_by_an_activity_helper_link_is_still_a_server_to_end() {
    use std::ffi::OsString;
    use strays::HELPER_NAME;
    let dir = tempfile::tempdir().unwrap();
    let server = dir
        .path()
        .join(format!("{SIDECAR_BIN}-aarch64-apple-darwin"));
    std::fs::write(&server, b"server").unwrap();
    let (mine, other) = (dir.path().join("mine"), dir.path().join("other"));
    std::fs::create_dir(&mine).unwrap();
    std::fs::create_dir(&other).unwrap();
    std::fs::hard_link(&server, mine.join(HELPER_NAME)).unwrap();
    std::fs::hard_link(&server, other.join(HELPER_NAME)).unwrap();
    let argv = |items: &[&std::ffi::OsStr]| -> Vec<OsString> {
        items.iter().map(|i| i.to_os_string()).collect()
    };
    let cmd = |root: &Path| argv(&[server.as_os_str(), "--root".as_ref(), root.as_os_str()]);
    assert!(runs_sidecar(Some(&server), &cmd(&mine)));
    assert!(runs_sidecar(Some(&mine.join(HELPER_NAME)), &cmd(&mine)));
    // Another root's link to the same binary.
    assert!(runs_sidecar(Some(&other.join(HELPER_NAME)), &cmd(&mine)));
    // Its own helper, though the started-from file was replaced by an update.
    let replaced = dir.path().join("replaced");
    std::fs::create_dir(&replaced).unwrap();
    std::fs::write(replaced.join(HELPER_NAME), b"older server").unwrap();
    assert!(runs_sidecar(
        Some(&replaced.join(HELPER_NAME)),
        &cmd(&replaced)
    ));
    // Two servers left on that older build: one named by the other's link to it.
    let sibling = dir.path().join("sibling");
    std::fs::create_dir(&sibling).unwrap();
    std::fs::hard_link(replaced.join(HELPER_NAME), sibling.join(HELPER_NAME)).unwrap();
    assert!(runs_sidecar(
        Some(&sibling.join(HELPER_NAME)),
        &cmd(&replaced)
    ));
    // A helper process is never a server, whatever the kernel names it by.
    let helper_run = |root: &Path| {
        argv(&[
            mine.join(HELPER_NAME).as_os_str(),
            "--activity-run".as_ref(),
            "tool".as_ref(),
            "--".as_ref(),
            "tool".as_ref(),
            "--root".as_ref(),
            root.as_os_str(),
        ])
    };
    assert!(!runs_sidecar(Some(&server), &helper_run(&mine)));
    assert!(!runs_sidecar(
        Some(&mine.join(HELPER_NAME)),
        &helper_run(&mine)
    ));
    // A server's options come first, and nothing after `--` is one.
    let late_root = argv(&[
        server.as_os_str(),
        "--".as_ref(),
        "--root".as_ref(),
        mine.as_os_str(),
    ]);
    assert!(!runs_sidecar(Some(&replaced.join(HELPER_NAME)), &late_root));
    // A relative argv path or root is not read against this process's directory.
    let relative = argv(&[
        format!("{SIDECAR_BIN}-aarch64-apple-darwin").as_ref(),
        "--root".as_ref(),
        mine.as_os_str(),
    ]);
    assert!(!runs_sidecar(Some(&mine.join(HELPER_NAME)), &relative));
    let relative_root = argv(&[server.as_os_str(), "--root".as_ref(), "replaced".as_ref()]);
    assert!(!runs_sidecar(
        Some(&replaced.join(HELPER_NAME)),
        &relative_root
    ));
    // A lookalike argv name is not a server's.
    let lookalike = dir.path().join(format!("{SIDECAR_BIN}X"));
    std::fs::hard_link(&server, &lookalike).unwrap();
    let lookalike_cmd = argv(&[lookalike.as_os_str(), "--root".as_ref(), mine.as_os_str()]);
    assert!(!runs_sidecar(
        Some(&other.join(HELPER_NAME)),
        &lookalike_cmd
    ));
    // Another file's helper outside the server's root, a wrapper, no exe, no argv: not a server.
    assert!(!runs_sidecar(
        Some(&replaced.join(HELPER_NAME)),
        &cmd(&mine)
    ));
    assert!(!runs_sidecar(
        Some(Path::new("/usr/bin/caffeinate")),
        &cmd(&mine)
    ));
    assert!(!runs_sidecar(None, &cmd(&mine)));
    assert!(!runs_sidecar(Some(&mine.join(HELPER_NAME)), &[]));
    // With no argv to read, a server's file name still decides.
    assert!(runs_sidecar(Some(&server), &[]));
}

/// `terminate_sidecar` decides on the kernel path and the argv it reads for the
/// pid; the argv must really be read, or a server named by a helper link
/// would be refused.
#[test]
fn the_end_check_reads_a_process_s_kernel_path_and_argv() {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let identity = process_identity(child.id());
    let _ = child.kill();
    let _ = child.wait();
    let (exe, cmd) = identity.expect("the child is found");
    // The kernel's path, which may be /bin/sleep resolved (merged /usr on Linux).
    let sleep = std::fs::canonicalize("/bin/sleep").unwrap();
    assert_eq!(
        exe.map(|exe| std::fs::canonicalize(exe).unwrap()),
        Some(sleep)
    );
    assert_eq!(cmd, ["/bin/sleep", "30"].map(std::ffi::OsString::from));
}

/// Only a terminal server binary is ever ended: bundled as the bare name or
/// built with its target triple, never a lookalike.
#[test]
fn only_a_terminal_server_binary_is_recognized() {
    assert!(is_sidecar_binary("codesense-session-server"));
    assert!(is_sidecar_binary(
        "codesense-session-server-aarch64-apple-darwin"
    ));
    for other in ["codesense-session-serverX", "codesense", "zsh", ""] {
        assert!(!is_sidecar_binary(other), "{other}");
    }
}

/// A host's layout transcript: the answer of every layout verb, and the
/// layout they leave, in order. Spawns go through `spawn`.
fn layout_transcript(
    host: &dyn TerminalDomain,
    spawn: &dyn Fn(&str) -> Result<(), String>,
) -> Vec<String> {
    use codesense_pty_core::domain::{ClientBootId, ViewerId};
    use codesense_pty_core::layout::{
        CloseMode, GridPreset, GridState, OpenTab, Placement, ViewHint, WorkspaceDescriptor,
    };
    let viewer = ViewerId::new("app");
    let boot = ClientBootId::new("boot");
    let (one, two) = (WorkspaceKey::new("ws-1"), WorkspaceKey::new("ws-2"));
    let described = |name: &str| WorkspaceDescriptor {
        display_name: name.into(),
        kind: "root".into(),
        ..WorkspaceDescriptor::default()
    };
    let open = |session: &str,
                workspace: &WorkspaceKey,
                descriptor: Option<WorkspaceDescriptor>| OpenTab {
        viewer: viewer.clone(),
        workspace: workspace.clone(),
        session: sid(session),
        descriptor,
        name: None,
        role: None,
        tags: Default::default(),
        placement: Placement::Tab,
        index: None,
        activate: true,
    };
    let layout = |host: &dyn TerminalDomain| {
        format!(
            "{:?}",
            host.layout_snapshot(&viewer, &boot)
                .map(|snapshot| (snapshot.workspaces, snapshot.viewer))
        )
    };
    let mut log = vec![layout(host)];
    let mut record = |answer: String| log.push(answer);
    record(format!(
        "{:?}",
        host.open_tab(
            open("a", &one, Some(described("one"))),
            Box::new(|| spawn("a"))
        )
    ));
    record(format!(
        "{:?}",
        host.open_tab(open("b", &one, None), Box::new(|| spawn("b")))
    ));
    record(format!(
        "{:?}",
        host.open_tab(open("a", &one, None), Box::new(|| spawn("a")))
    ));
    record(format!(
        "{:?}",
        host.open_tab(open("c", &two, None), Box::new(|| spawn("c")))
    ));
    record(format!(
        "{:?}",
        host.open_tab(
            open("d", &one, None),
            Box::new(|| Err("spawn_pty: no shell".into()))
        )
    ));
    record(format!("{:?}", host.rename_tab(&sid("a"), "build".into())));
    record(format!("{:?}", host.rename_tab(&sid("ghost"), "x".into())));
    record(format!("{:?}", host.move_tab(&sid("b"), 0)));
    record(format!(
        "{:?}",
        host.set_tags(
            &sid("a"),
            [("launcher".to_string(), Some("mcp".to_string()))].into()
        )
    ));
    record(format!(
        "{:?}",
        host.set_view_hint(
            &viewer,
            &sid("a"),
            ViewHint {
                scroll_from_bottom: 3
            }
        )
    ));
    record(format!(
        "{:?}",
        host.set_active(&viewer, &one, Some(&sid("b")))
    ));
    record(format!(
        "{:?}",
        host.set_grid(
            &one,
            GridState {
                paned: vec![sid("a"), sid("b")],
                preset: GridPreset::Rows,
                collapsed: false,
            }
        )
    ));
    record(format!(
        "{:?}",
        host.set_descriptor(&one, described("renamed"))
    ));
    record(format!(
        "{:?}",
        host.set_descriptor(&two, described("absent"))
    ));
    record(format!(
        "{:?}",
        host.place_tab(
            &sid("b"),
            &two,
            Placement::Background,
            None,
            Some(described("two"))
        )
    ));
    log.push(layout(host));
    log.push(format!(
        "{:?}",
        host.close_tab(&viewer, &sid("a"), CloseMode::Kill)
    ));
    log.push(format!("{:?}", host.close_workspace(&two)));
    log.push(format!("{:?}", host.close_workspace(&one)));
    log.push(layout(host));
    log
}

/// `terminal_respawn_tab` on either host: a tab whose process runs is refused
/// (its scope config untouched); an exited one is reaped before the spawn
/// runs — its old scope config already gone by then, and the host told a
/// reap, not a kill — and comes back live under the same id, in the same
/// place.
#[test]
fn respawn_tab_refuses_a_running_tab_and_respawns_an_exited_one_in_place() {
    let _home_env = crate::routines::test_support::TempHome::new();
    use crate::commands::mcp_scope::{mcp_scope_session_dir, remove_scope_config};
    use crate::commands::pty_backend::respawn_with_fresh_scope;
    use codesense_pty_core::domain::{ClientBootId, ViewerId};
    use codesense_pty_core::layout::{EntryState, OpenTab, Placement, WorkspaceDescriptor};

    let tag = std::process::id();
    let viewer = ViewerId::new("app");
    let project = |id: &str| {
        let dir = mcp_scope_session_dir(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".mcp.json"), "{}").unwrap();
        dir
    };
    let tabs = |host: &dyn TerminalDomain, workspace: &WorkspaceKey| {
        host.layout_snapshot(&viewer, &ClientBootId::new("boot"))
            .unwrap()
            .workspaces
            .get(workspace)
            .map(|ws| {
                ws.tabs
                    .iter()
                    .map(|t| (t.session.as_str().to_string(), t.state.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let local_events = Arc::new(RecordingSink::default());
    let local = TerminalHost::new(
        ServerIdentity::ephemeral("test"),
        local_events.clone(),
        runtime.handle().clone(),
    );
    local.attach_echo_emulator();
    let root = short_root();
    let sidecar = TestServer::start_at(root.path());
    let remote_events = Arc::new(RecordingSink::default());
    let client =
        SessionServerClient::connect(root.path(), remote_events.clone(), Arc::default(), None)
            .unwrap();

    let spawn_on = |name: &str, id: &str, script: &str| -> Result<(), String> {
        if name == "local" {
            let pty = runtime
                .block_on(LocalPtyRunner.spawn(sh_spec(id, script)))
                .map_err(String::from)?;
            local.register(pty, registration(id, None, false))
        } else {
            sidecar.track(id);
            client
                .spawn(sh(script), registration(id, None, false))
                .map(|_| ())
                .map_err(|failure| failure.message)
        }
    };

    for (host, events, name) in [
        (&local as &dyn TerminalDomain, &local_events, "local"),
        (
            client.as_ref() as &dyn TerminalDomain,
            &remote_events,
            "sidecar",
        ),
    ] {
        let workspace = WorkspaceKey::new(format!("respawn-{name}"));
        let running = format!("respawn-running-{name}-{tag}");
        let ended = format!("respawn-ended-{name}-{tag}");
        let after = format!("respawn-after-{name}-{tag}");
        // Ask once first: a first read under a boot id hides the entries already
        // exited, and `exit 3` may be over before the next one.
        tabs(host, &workspace);
        for (id, script) in [
            (&running, "sleep 60"),
            (&ended, "printf 'HISTORY-X\\n'; exit 3"),
            (&after, "sleep 60"),
        ] {
            host.open_tab(
                OpenTab {
                    viewer: viewer.clone(),
                    workspace: workspace.clone(),
                    session: sid(id),
                    descriptor: Some(WorkspaceDescriptor {
                        display_name: "respawn".into(),
                        kind: "root".into(),
                        ..WorkspaceDescriptor::default()
                    }),
                    name: None,
                    role: None,
                    tags: Default::default(),
                    placement: Placement::Tab,
                    index: None,
                    activate: false,
                },
                Box::new(|| spawn_on(name, id, script)),
            )
            .unwrap();
        }
        assert!(
            wait_until(|| tabs(host, &workspace)[1].1.is_exited()),
            "{name}: the ended tab shows exited"
        );
        let before: Vec<String> = tabs(host, &workspace).into_iter().map(|t| t.0).collect();

        // A running tab is refused, and nothing of it is touched.
        let running_dir = project(&running);
        let refused = respawn_with_fresh_scope(host, &sid(&running), || {
            panic!("{name}: a refused respawn never spawns")
        });
        let kept = running_dir.exists();
        remove_scope_config(&running);
        assert!(
            refused.as_ref().is_err_and(|e| e.contains("still running")),
            "{name}: {refused:?}"
        );
        assert!(kept, "{name}: a refused respawn keeps the scope config");
        assert_eq!(host.has_session(&sid(&running)), Ok(true), "{name}");

        // The exited tab: reaped, its old scope gone, before the spawn runs.
        let ended_dir = project(&ended);
        let mut at_spawn = None;
        let mut second = None;
        let entry = respawn_with_fresh_scope(host, &sid(&ended), || {
            at_spawn = Some((ended_dir.exists(), host.has_session(&sid(&ended))));
            // A second restart while this one runs is refused, never doubled.
            second = Some(respawn_with_fresh_scope(host, &sid(&ended), || {
                panic!("{name}: a second restart never spawns")
            }));
            spawn_on(name, &ended, "sleep 60; :")
        })
        .unwrap();
        assert!(
            second
                .as_ref()
                .is_some_and(|r| r.as_ref().is_err_and(|e| e.contains("already in progress"))),
            "{name}: {second:?}"
        );
        assert_eq!(
            at_spawn,
            Some((false, Ok(false))),
            "{name}: at the spawn the old scope config and session are gone"
        );
        assert_eq!(entry.session, sid(&ended), "{name}: the same id");
        assert_eq!(entry.state, EntryState::Live, "{name}: exited → live");
        let now = tabs(host, &workspace);
        assert_eq!(
            now.iter().map(|t| t.0.clone()).collect::<Vec<_>>(),
            before,
            "{name}: the same place"
        );
        assert_eq!(now[1].1, EntryState::Live, "{name}");
        // The terminal carried: the ended session's output is still there.
        let text = host
            .read_terminal_text(
                &sid(&ended),
                u32::MAX,
                codesense_pty_core::emulator::TextMode::Normal,
            )
            .unwrap()
            .text;
        assert!(text.starts_with("HISTORY-X"), "{name}: {text:?}");
        assert!(
            wait_until(|| events
                .events()
                .contains(&TerminalDomainEvent::SessionRemoved {
                    session: sid(&ended),
                    reason: TeardownReason::Reap,
                })),
            "{name}: the host hears a reap, not a kill"
        );
        assert!(!events
            .events()
            .contains(&TerminalDomainEvent::SessionRemoved {
                session: sid(&ended),
                reason: TeardownReason::Kill,
            }));
        // A live tab again: refused.
        assert!(respawn_with_fresh_scope(host, &sid(&ended), || Ok(())).is_err());

        for id in [&running, &ended, &after] {
            let _ = host.kill(&sid(id));
        }
    }
}

/// Every layout verb works through the sidecar exactly as in-process — the
/// same answers, refusals and resulting layout from the same model — and each
/// change is pushed to the app, stamped with the sidecar's epoch.
#[test]
fn every_layout_verb_crosses_the_socket_as_in_process() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let local = TerminalHost::new(
        ServerIdentity::ephemeral("test"),
        Arc::new(RecordingSink::default()),
        runtime.handle().clone(),
    );
    let in_process = layout_transcript(&local, &|id| {
        let pty = runtime
            .block_on(LocalPtyRunner.spawn(sh_spec(id, "sleep 60; :")))
            .map_err(String::from)?;
        local.register(pty, registration(id, None, false))
    });

    let sidecar = TestServer::start();
    let events = Arc::new(RecordingSink::default());
    let client = SessionServerState::default()
        .connect_or_launch(sidecar.root(), events.clone(), no_launch)
        .unwrap();
    let over_the_wire = layout_transcript(&*client, &|id| {
        sidecar.track(id);
        client
            .spawn(sh("sleep 60; :"), registration(id, None, false))
            .map(|_| ())
            .map_err(|failure| failure.message)
    });
    assert_eq!(over_the_wire, in_process);
    assert!(
        over_the_wire[1].starts_with("Ok(TabEntry"),
        "{over_the_wire:?}"
    );
    assert!(
        over_the_wire[3].contains("AlreadyPlaced"),
        "{over_the_wire:?}"
    );

    let epoch = sidecar.host().terminal().identity().unwrap().epoch;
    assert!(
        events.events().iter().any(|event| matches!(
            event,
            TerminalDomainEvent::LayoutChanged { epoch: pushed, .. } if *pushed == epoch
        )),
        "layout changes are pushed to the app"
    );
}

/// SW-RT-6d (AC-6): a read that must not guess learns whether the sidecar may
/// hold sessions without ever waiting on it: none runs (absent — it holds
/// nothing), one runs unconnected (unknown), a connect or launch is in
/// progress (unknown, at once), or connected. An app with no instance of its
/// own never probes one.
#[test]
fn presence_is_answered_without_waiting_on_the_sidecar() {
    let root = short_root();
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    assert!(matches!(state.presence(), Presence::Absent), "nothing runs");

    let sidecar = TestServer::start_at(root.path());
    assert!(
        matches!(state.presence(), Presence::Unknown(_)),
        "it runs but is not connected: what it holds is unknown"
    );
    let client = state
        .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    assert!(matches!(state.presence(), Presence::Connected(_)));

    client.sever();
    assert!(wait_until(|| !client.is_alive()));
    let connecting = lock(&state.attempts);
    let asked = Instant::now();
    assert!(
        matches!(state.presence(), Presence::Unknown(_)),
        "a connect in progress is never waited on"
    );
    assert!(asked.elapsed() < Duration::from_secs(1));
    drop(connecting);
    drop(sidecar);

    assert!(matches!(
        SessionServerState::default().presence(),
        Presence::Absent
    ));
}

/// A lost connection is told to the app, which reconnects in the background
/// so the panes viewing its sessions resume.
#[test]
fn a_lost_connection_is_told() {
    let root = short_root();
    let _sidecar = TestServer::start_at(root.path());
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let told = Arc::new(AtomicU64::new(0));
    let counter = told.clone();
    state.on_lost(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let client = state
        .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), no_launch)
        .unwrap();
    assert_eq!(told.load(Ordering::SeqCst), 0);
    client.sever();
    assert!(wait_until(|| told.load(Ordering::SeqCst) == 1));
}

/// A respawn of a session id gets an input channel of its own, so the late
/// close of the id's earlier channel — its old incarnation leaving — never
/// closes the new session's input.
#[test]
fn a_respawn_keeps_its_own_input_channel() {
    let sidecar = TestServer::start();
    let events = Arc::new(RecordingSink::default());
    let client =
        SessionServerClient::connect(sidecar.root(), events.clone(), Arc::default(), None).unwrap();
    let echo = "while read line; do printf 'GOT=%s\\n' \"$line\"; done";
    spawn(&sidecar, &client, echo, registration("same", None, false));
    client.write(&sid("same"), b"one\r").unwrap();
    let old = client
        .input_channel(&sid("same"))
        .expect("the first channel");
    spawn(&sidecar, &client, echo, registration("same", None, false));
    assert_eq!(
        client.input_channel(&sid("same")),
        None,
        "the old incarnation's channel closed before the respawn"
    );
    client.write(&sid("same"), b"two\r").unwrap();
    let new = client.input_channel(&sid("same")).expect("the new channel");
    assert_ne!(new, old);
    // The old incarnation's removal is handled late: only its own channel closes.
    client.close_input(&sid("same"), Some(old));
    assert_eq!(client.input_channel(&sid("same")), Some(new));
    client.write(&sid("same"), b"three\r").unwrap();
    assert!(wait_until(|| sidecar.text("same").contains("GOT=three")));
}

/// A viewport attached for a subscribe whose caller stopped waiting — its
/// reply arriving too late — is detached again at once, not left attached in
/// the sidecar until the connection closes.
#[test]
fn a_late_subscribe_reply_detaches_its_viewport() {
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let hello = mock_hello(instance_id(root.path()));
    let host = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        greet_client(&stream, &hello, 0);
        let mut seen = Vec::new();
        while let Some((rid, request)) = next_call(&stream) {
            match request {
                Request::Listen => answer(&stream, rid, Ok(Reply::Done)),
                Request::HasSession { .. } => {
                    // A subscribe reply nobody waits for any more, then the answer.
                    let late = SubscribeResult {
                        snapshot: codesense_pty_core::emulator::TerminalSnapshot::of_text("", 0),
                        stream_id: StreamId(7),
                        story_id: None,
                        workspace_id: None,
                        epic_id: None,
                    };
                    answer(
                        &stream,
                        rid + 1000,
                        Ok(Reply::Subscribed {
                            result: Box::new(late),
                        }),
                    );
                    answer(&stream, rid, Ok(Reply::Flag { value: true }));
                }
                other => {
                    seen.push(other);
                    answer(&stream, rid, Ok(Reply::Flag { value: true }));
                    return seen;
                }
            }
        }
        seen
    });
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    assert_eq!(client.has_session(&sid("s")), Ok(true));
    let seen = host.join().unwrap();
    assert!(
        matches!(
            seen.as_slice(),
            [Request::Unsubscribe {
                stream: StreamId(7)
            }]
        ),
        "{seen:?}"
    );
}

/// A sidecar that runs but will not complete the handshake (hung) is ended by
/// a restart — found by the pid it recorded under the instance's lock — and
/// this build's launched in its place; how many sessions it ran is unknown.
#[test]
fn a_restart_ends_a_hung_sidecar() {
    use std::os::fd::AsRawFd;
    let root = short_root();
    // The hung sidecar: it holds the instance's lock with its pid recorded, and
    // its socket accepts connections it never answers.
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(codesense_pty_core::protocol::lock_path(root.path()))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    std::fs::write(codesense_pty_core::protocol::lock_path(root.path()), "4242").unwrap();
    let hung = UnixListener::bind(socket_path(root.path())).unwrap();
    assert_eq!(sidecar_to_end(root.path()).unwrap(), Some((4242, None)));

    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let ended = std::cell::RefCell::new(Some((hung, lock_file)));
    let served = std::cell::RefCell::new(None);
    let (client, count) = state
        .restart(
            root.path(),
            Arc::new(RecordingSink::default()),
            |pid| {
                assert_eq!(pid, 4242, "the recorded pid is ended");
                drop(ended.borrow_mut().take());
                Ok(())
            },
            |root| {
                *served.borrow_mut() = Some(TestServer::start_at(root));
                Ok(())
            },
        )
        .expect("restarted");
    assert_eq!(count, None, "a hung sidecar never said");
    assert!(client.is_alive());
}

/// SW-RT-6d (AC-2): sidecar mode is the default — no switch gates it anywhere.
#[test]
fn no_switch_gates_the_sidecar() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    rust_sources(&manifest.join("src"), &mut sources);
    rust_sources(&manifest.join("crates"), &mut sources);
    let switch = ["CODESENSE_PTY", "_SIDECAR"].concat();
    for source in sources {
        let text = std::fs::read_to_string(&source).unwrap();
        assert!(!text.contains(&switch), "{source:?} reads the old switch");
    }
}

/// The app hands the sidecar it launches the resources it ships: the
/// sidecar's spawns resolve shell integration from them against its own
/// environment (`$SHELL`, `HOME`), as an in-process spawn does.
#[test]
fn a_launched_sidecar_resolves_spawns_against_the_apps_resources() {
    let binary = sidecar_binary();
    let root = short_root();
    let _reap = ReapSidecarsAt::new(root.path());
    let home = tempfile::tempdir().unwrap();
    let resources = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(resources.path().join("shell-integration")).unwrap();
    std::fs::write(
        resources
            .path()
            .join("shell-integration/codesense-shell-init.zsh"),
        "print \"RT6D-INIT=$__CODESENSE_SHELL_INIT\"\n",
    )
    .unwrap();
    let state = SessionServerState::default();
    let events = Arc::new(RecordingSink::default());
    let client = state
        .connect_or_launch(root.path(), events.clone(), |root| {
            launch_detached_with_env(
                &binary,
                root,
                Some(resources.path()),
                None,
                &[
                    tie_to_this_process(),
                    ("SHELL", "/bin/zsh".to_string()),
                    ("HOME", home.path().display().to_string()),
                ],
            )
        })
        .expect("the launched sidecar connects");
    let intent = codesense_pty_core::spawn::SpawnIntent {
        remote_launch: Default::default(),
        command: None,
        argv: None,
        shell: None,
        cwd: String::new(),
        cols: 80,
        rows: 24,
        shell_integration: true,
        starship_preset: Some("plain".to_string()),
        api_port: None,
        env_remove: Vec::new(),
        env: Vec::new(),
        agent_env: Vec::new(),
        login_path: None,
    };
    client
        .spawn(intent, registration("integrated", None, false))
        .expect("spawned");
    let expected = format!(
        "RT6D-INIT={}",
        resources
            .path()
            .join("shell-integration/codesense-shell-init.zsh")
            .display()
    );
    assert!(
        wait_until(|| events.output("integrated").contains(&expected)),
        "the sidecar's shell sourced the app's shell-init"
    );
    client.kill(&sid("integrated")).unwrap();
}

/// SW-AGR-19 (AC6): This Mac's update stops only the server and sessions the
/// person confirmed, then launches this build's: a changed session set is
/// refused before anything stops.
#[test]
fn agr19_this_mac_update_refuses_a_changed_review() {
    let root = short_root();
    let server = mock_sidecar(root.path(), 2, |_, stream, rid| {
        answer(
            stream,
            rid,
            Ok(Reply::Sessions {
                sessions: [sid("existing")].into_iter().collect(),
            }),
        );
    });
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let mut reviewed = state.local_server_identity().unwrap().unwrap();
    assert_eq!(reviewed.session_ids, ["existing"]);
    reviewed.session_ids.clear();
    let failure = state
        .replace_local_server(
            root.path(),
            &reviewed,
            Arc::new(RecordingSink::default()),
            |_| panic!("a stale review cannot stop any process"),
            |_| panic!("a stale review cannot launch a replacement"),
        )
        .err()
        .unwrap();
    assert!(!failure.old_stopped);
    assert!(failure.error.to_string().contains("changed"));
    server.join().unwrap();
}

#[test]
fn agr19_this_mac_update_replaces_the_confirmed_server() {
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let hello = Hello {
        pid: 4243,
        server_id: "reviewed-server".into(),
        epoch: "reviewed-epoch".into(),
        // A server with everything, activity included: the update is about
        // its build, not a capability.
        capabilities: vec![
            Capability::Terminals,
            Capability::ActivityV1,
            Capability::ActivityV2,
        ],
        ..mock_hello(instance_id(root.path()))
    };
    let old = std::thread::spawn(move || {
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            greet_client(&stream, &hello, 1);
            let (rid, request) = next_call(&stream).unwrap();
            assert!(matches!(request, Request::LiveSessionIds));
            answer(
                &stream,
                rid,
                Ok(Reply::Sessions {
                    sessions: [sid("reviewed-terminal")].into_iter().collect(),
                }),
            );
        }
    });
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let expected = state.local_server_identity().unwrap().unwrap();
    assert_eq!(expected.pid, 4243);
    let stopped = std::cell::Cell::new(false);
    let replacement = Mutex::new(None);
    let (client, old_stopped) = state
        .replace_local_server(
            root.path(),
            &expected,
            Arc::new(RecordingSink::default()),
            |pid| {
                assert_eq!(pid, 4243);
                old.join().unwrap();
                std::fs::remove_file(socket_path(root.path())).unwrap();
                stopped.set(true);
                Ok(())
            },
            |root| {
                *lock(&replacement) = Some(TestServer::start_at(root));
                Ok(())
            },
        )
        .unwrap();
    assert!(stopped.get());
    assert!(old_stopped);
    assert_ne!(client.epoch(), expected.epoch);
}

/// SV-7: Stop ends the confirmed server — its process, and so its sessions —
/// and launches nothing in its place; a review that no longer matches stops
/// nothing.
#[test]
fn sv7_stop_ends_the_confirmed_server_and_launches_nothing() {
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let hello = Hello {
        pid: 4245,
        server_id: "reviewed-server".into(),
        epoch: "reviewed-epoch".into(),
        ..mock_hello(instance_id(root.path()))
    };
    let old = std::thread::spawn(move || {
        for _ in 0..3 {
            let (stream, _) = listener.accept().unwrap();
            greet_client(&stream, &hello, 1);
            let (rid, request) = next_call(&stream).unwrap();
            assert!(matches!(request, Request::LiveSessionIds));
            answer(
                &stream,
                rid,
                Ok(Reply::Sessions {
                    sessions: [sid("reviewed-terminal")].into_iter().collect(),
                }),
            );
        }
    });
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let expected = state.local_server_identity().unwrap().unwrap();
    assert_eq!(expected.session_ids, ["reviewed-terminal"]);

    let mut stale = expected.clone();
    stale.session_ids.clear();
    let refused = state
        .stop_local_server(
            root.path(),
            &stale,
            Arc::new(RecordingSink::default()),
            |_| panic!("a stale review stops nothing"),
        )
        .unwrap_err();
    assert!(!refused.old_stopped);
    assert!(refused.error.to_string().contains("changed"));

    let stopped = std::cell::Cell::new(false);
    let old_stopped = state
        .stop_local_server(
            root.path(),
            &expected,
            Arc::new(RecordingSink::default()),
            |pid| {
                assert_eq!(pid, 4245);
                old.join().unwrap();
                std::fs::remove_file(socket_path(root.path())).unwrap();
                stopped.set(true);
                Ok(())
            },
        )
        .unwrap();
    assert!(stopped.get() && old_stopped);
    assert!(state.connected().is_none());
    assert!(
        !socket_path(root.path()).exists(),
        "nothing is launched in its place"
    );
}

#[test]
fn agr19_this_mac_update_launches_once_more_when_the_new_server_does_not_start() {
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let hello = Hello {
        pid: 4244,
        server_id: "reviewed-server".into(),
        epoch: "reviewed-epoch".into(),
        ..mock_hello(instance_id(root.path()))
    };
    let old = std::thread::spawn(move || {
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            greet_client(&stream, &hello, 1);
            let (rid, request) = next_call(&stream).unwrap();
            assert!(matches!(request, Request::LiveSessionIds));
            answer(
                &stream,
                rid,
                Ok(Reply::Sessions {
                    sessions: Default::default(),
                }),
            );
        }
    });
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let expected = state.local_server_identity().unwrap().unwrap();
    let launches = std::cell::Cell::new(0);
    let replacement = Mutex::new(None);
    let (client, old_stopped) = state
        .replace_local_server(
            root.path(),
            &expected,
            Arc::new(RecordingSink::default()),
            |_| {
                old.join().unwrap();
                std::fs::remove_file(socket_path(root.path())).unwrap();
                Ok(())
            },
            |root| {
                launches.set(launches.get() + 1);
                if launches.get() == 1 {
                    return Err("the first launch failed".into());
                }
                *lock(&replacement) = Some(TestServer::start_at(root));
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(launches.get(), 2, "one more launch after the first failed");
    assert!(old_stopped);
    assert_ne!(client.epoch(), expected.epoch);
}

/// Hold `root`'s singleton lock as a running server does.
fn hold_instance_lock(root: &Path) -> std::fs::File {
    use std::os::fd::AsRawFd;
    let path = codesense_pty_core::protocol::lock_path(root);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .unwrap();
    // SAFETY: `file` is open for the call; the lock is released as it closes.
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    file
}

/// An old server answering its identity reads (`calls` connections) on
/// `root`, holding the instance lock as a real one does.
fn old_server(root: &Path, pid: u32, calls: usize) -> (std::thread::JoinHandle<()>, std::fs::File) {
    let lock = hold_instance_lock(root);
    let listener = UnixListener::bind(socket_path(root)).unwrap();
    let hello = Hello {
        pid,
        server_id: "reviewed-server".into(),
        epoch: "reviewed-epoch".into(),
        ..mock_hello(instance_id(root))
    };
    let serving = std::thread::spawn(move || {
        for _ in 0..calls {
            let (stream, _) = listener.accept().unwrap();
            greet_client(&stream, &hello, 1);
            let (rid, request) = next_call(&stream).unwrap();
            assert!(matches!(request, Request::LiveSessionIds));
            answer(
                &stream,
                rid,
                Ok(Reply::Sessions {
                    sessions: [sid("reviewed-terminal")].into_iter().collect(),
                }),
            );
        }
    });
    (serving, lock)
}

#[test]
fn agr19_after_stop_launches_once_released_and_reattaches_only_while_answering() {
    // Let go in time, or just after the wait: launch this build's.
    assert_eq!(after_stop(Ok(())), Ok(true));
    assert_eq!(after_stop(Err(("slow".into(), false, false))), Ok(true));
    // Still holding its lock: its sessions are ending; not reattached once
    // it stops answering, held again while it still answers.
    assert_eq!(
        after_stop(Err(("slow".into(), true, false))),
        Err(("slow".into(), true, false))
    );
    assert_eq!(
        after_stop(Err(("slow".into(), false, true))),
        Err(("slow".into(), true, true))
    );
}

#[test]
fn agr19_this_mac_update_launches_only_once_the_old_server_lets_go_of_its_lock() {
    let root = short_root();
    let (old, held) = old_server(root.path(), 4245, 2);
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let expected = state.local_server_identity().unwrap().unwrap();
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let held = Mutex::new(Some(held));
    let replacement = Mutex::new(None);
    let (client, old_stopped) = state
        .replace_local_server(
            root.path(),
            &expected,
            Arc::new(RecordingSink::default()),
            |_| {
                old.join().unwrap();
                // The socket goes first; the lock a moment later, as a
                // server that is still exiting.
                std::fs::remove_file(socket_path(root.path())).unwrap();
                let file = lock(&held).take();
                let released = released.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(200));
                    released.store(true, Ordering::SeqCst);
                    drop(file);
                });
                Ok(())
            },
            |root| {
                assert!(
                    released.load(Ordering::SeqCst),
                    "never launched while the old server holds the lock"
                );
                *lock(&replacement) = Some(TestServer::start_at(root));
                Ok(())
            },
        )
        .unwrap();
    assert!(old_stopped);
    assert_ne!(client.epoch(), expected.epoch);
}

#[test]
fn agr19_this_mac_update_keeps_the_old_server_when_it_cannot_be_stopped() {
    let root = short_root();
    let (old, _held) = old_server(root.path(), 4246, 2);
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let expected = state.local_server_identity().unwrap().unwrap();
    let failure = state
        .replace_local_server(
            root.path(),
            &expected,
            Arc::new(RecordingSink::default()),
            |_| Err("not permitted".into()),
            |_| panic!("nothing is launched while the old server runs"),
        )
        .err()
        .expect("refused");
    assert!(
        !failure.old_stopped,
        "the old server and its sessions still run"
    );
    assert!(failure.error.to_string().contains("not permitted"));
    old.join().unwrap();
}

/// SW-AGR-19 (AC6), the real thing: This Mac's built terminal server, with
/// a live session, is replaced by the confirmed update — it stops (SIGTERM),
/// its lock is released, a new server starts on the same root and the app
/// is connected to it; the old session is gone.
#[test]
fn agr19_this_mac_update_replaces_a_real_terminal_server() {
    let binary = sidecar_binary();
    let root = short_root();
    let _reap = ReapSidecarsAt::new(root.path());
    let launch =
        |root: &Path| launch_detached_with_env(&binary, root, None, None, &[tie_to_this_process()]);
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let old = state
        .connect_or_launch(root.path(), Arc::new(RecordingSink::default()), launch)
        .expect("the old server runs");
    old.spawn(sh("sleep 600"), registration("confirmed", None, false))
        .expect("a live session");
    let old_pid = old.pid();
    let expected = state.local_server_identity().unwrap().unwrap();
    assert_eq!(expected.session_ids, ["confirmed"]);
    let (client, old_stopped) = state
        .replace_local_server(
            root.path(),
            &expected,
            Arc::new(RecordingSink::default()),
            |pid| {
                // SAFETY: signalling the confirmed server's pid.
                (unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0)
                    .then_some(())
                    .ok_or_else(|| std::io::Error::last_os_error().to_string())
            },
            launch,
        )
        .expect("replaced");
    assert!(old_stopped);
    assert!(wait_until(|| !alive(old_pid)), "the old server is gone");
    assert_ne!(client.pid(), old_pid, "a new server");
    assert_ne!(client.epoch(), expected.epoch);
    let now = state.local_server_identity().unwrap().unwrap();
    assert!(
        now.session_ids.is_empty(),
        "the confirmed session ended with it"
    );
}

#[test]
fn activity_upgrade_review_is_read_only_and_checks_the_exact_session_set() {
    let root = short_root();
    let server = mock_sidecar(root.path(), 2, |_, stream, rid| {
        answer(
            stream,
            rid,
            Ok(Reply::Sessions {
                sessions: [sid("existing")].into_iter().collect(),
            }),
        );
    });
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let mut reviewed = state.activity_upgrade_status().unwrap().unwrap();
    assert_eq!(reviewed.session_ids, ["existing"]);
    reviewed.session_ids.clear();
    let failure = state
        .update_activity_server(
            root.path(),
            &reviewed,
            Arc::new(RecordingSink::default()),
            |_| panic!("stale review cannot stop any process"),
            |_| panic!("stale review cannot launch a replacement"),
        )
        .err()
        .unwrap();
    assert!(!failure.old_stopped);
    assert!(failure.error.to_string().contains("changed"));
    assert!(server
        .join()
        .unwrap()
        .iter()
        .all(|request| matches!(request, Request::LiveSessionIds)));
}

#[test]
fn activity_upgrade_review_accepts_only_a_verified_capable_owner_as_current() {
    let root = short_root();
    let hello = Hello {
        capabilities: vec![
            Capability::Terminals,
            Capability::ActivityV1,
            Capability::ActivityV2,
            Capability::InboxWakeHoldsV1,
        ],
        ..mock_hello(instance_id(root.path()))
    };
    let server = mock_host(root.path(), hello, 0);
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    assert!(state.activity_upgrade_status().unwrap().is_none());
    server.join().unwrap().unwrap();
    assert!(
        state.activity_upgrade_status().is_err(),
        "unreachable is not already updated"
    );
}

#[test]
fn activity_upgrade_review_reports_an_owner_without_spend_holds_as_not_current() {
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let hello = Hello {
        capabilities: vec![Capability::Terminals, Capability::ActivityV2],
        ..mock_hello(instance_id(root.path()))
    };
    let owner = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        greet_client(&stream, &hello, 1);
        let (rid, request) = next_call(&stream).unwrap();
        assert!(matches!(request, Request::LiveSessionIds));
        answer(
            &stream,
            rid,
            Ok(Reply::Sessions {
                sessions: [sid("held-terminal")].into_iter().collect(),
            }),
        );
    });
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let upgrade = state.activity_upgrade_status().unwrap();
    owner.join().unwrap();
    assert_eq!(upgrade.unwrap().session_ids, vec!["held-terminal".to_string()]);
}

#[test]
fn activity_upgrade_confirmed_owner_is_replaced_and_snapshot_verified() {
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let hello = Hello {
        pid: 4242,
        server_id: "reviewed-server".into(),
        epoch: "reviewed-epoch".into(),
        ..mock_hello(instance_id(root.path()))
    };
    let old = std::thread::spawn(move || {
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            greet_client(&stream, &hello, 1);
            let (rid, request) = next_call(&stream).unwrap();
            assert!(matches!(request, Request::LiveSessionIds));
            answer(
                &stream,
                rid,
                Ok(Reply::Sessions {
                    sessions: [sid("reviewed-terminal")].into_iter().collect(),
                }),
            );
        }
    });
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let expected = state.activity_upgrade_status().unwrap().unwrap();
    let stopped = std::cell::Cell::new(false);
    let replacement = Mutex::new(None);
    let (client, old_stopped) = state
        .update_activity_server(
            root.path(),
            &expected,
            Arc::new(RecordingSink::default()),
            |pid| {
                assert_eq!(pid, 4242);
                assert!(state.connected().is_none());
                old.join().unwrap();
                std::fs::remove_file(socket_path(root.path())).unwrap();
                stopped.set(true);
                Ok(())
            },
            |root| {
                let server = TestServer::start_at(root);
                server.host().start_activity(root).unwrap();
                *lock(&replacement) = Some(server);
                Ok(())
            },
        )
        .unwrap();
    assert!(stopped.get());
    assert!(old_stopped);
    assert_ne!(client.epoch(), expected.epoch);
    assert!(client.supports_activity());
    assert!(client.activity_snapshot().unwrap().is_empty());
    // A lost success acknowledgement is safe to retry: the capable replacement
    // is verified, not stopped, even while carrying the old review identity.
    state
        .update_activity_server(
            root.path(),
            &expected,
            Arc::new(RecordingSink::default()),
            |_| panic!("must not stop the replacement"),
            |_| panic!("must not relaunch the replacement"),
        )
        .unwrap();
}

#[test]
fn activity_upgrade_retry_after_old_owner_gone_only_starts_replacement() {
    let root = short_root();
    let state = SessionServerState::at(Some(root.path().to_path_buf()));
    let expected = LocalActivityUpgrade {
        server_id: "old".into(),
        epoch: "old-epoch".into(),
        pid: 4242,
        session_ids: vec!["previous-terminal".into()],
    };
    let replacement = Mutex::new(None);
    state
        .update_activity_server(
            root.path(),
            &expected,
            Arc::new(RecordingSink::default()),
            |_| panic!("absent owner cannot authorize a termination"),
            |root| {
                let server = TestServer::start_at(root);
                server.host().start_activity(root).unwrap();
                *lock(&replacement) = Some(server);
                Ok(())
            },
        )
        .unwrap();
    assert!(state.connected().unwrap().supports_activity());
}

/// A terminal server from before the prompt-segment request is never sent it:
/// the caption keeps its cwd and home and leaves the Git segment empty.
#[test]
fn a_session_prompt_on_an_older_terminal_server_is_not_sent() {
    let root = short_root();
    let mock = mock_sidecar(root.path(), 1, |_, _, _| {});
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    assert_eq!(client.session_prompt(&sid("old-server")), Ok(None));
    drop(client);
    assert!(mock.join().unwrap().is_empty(), "nothing was sent");
}

/// A capable terminal server (a remote Machine's included) is asked for the
/// session's prompt by its id, and its own reading — cwd, home, Git status —
/// comes back as it sent it.
#[test]
fn a_session_prompt_asks_the_capable_host_for_that_session() {
    use codesense_pty_core::git_prompt::{GitPromptStatus, SessionPrompt};
    let root = short_root();
    let listener = UnixListener::bind(socket_path(root.path())).unwrap();
    let hello = Hello {
        capabilities: vec![Capability::Terminals, Capability::SessionPrompt],
        ..mock_hello(instance_id(root.path()))
    };
    let prompt = SessionPrompt {
        cwd: Some("/home/dev/repo/src".into()),
        home: Some("/home/dev".into()),
        git: Some(GitPromptStatus {
            branch: Some("main".into()),
            sha: Some("0123456789abcdef0123456789abcdef01234567".into()),
            modified: 2,
            behind: 1,
            ..Default::default()
        }),
    };
    let sent = prompt.clone();
    let host = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        greet_client(&stream, &hello, 0);
        let mut seen = Vec::new();
        while let Some((rid, request)) = next_call(&stream) {
            match request {
                Request::Listen => answer(&stream, rid, Ok(Reply::Done)),
                request => {
                    seen.push(request);
                    answer(
                        &stream,
                        rid,
                        Ok(Reply::Prompt {
                            prompt: Box::new(sent.clone()),
                        }),
                    );
                    break;
                }
            }
        }
        seen
    });
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    assert_eq!(client.session_prompt(&sid("remote-tab")), Ok(Some(prompt)));
    drop(client);
    let seen = host.join().unwrap();
    assert!(
        matches!(seen.as_slice(), [Request::SessionPrompt { session }] if session == &sid("remote-tab"))
    );
}

/// SW-AGR-19 (AC8): with this app attached to one root's server and another
/// server running on a second root, only the other one is a stray — listed
/// with its root, start time and sessions — and it stops after its own
/// confirmation (its pid and start time checked again).
#[test]
fn agr19_only_the_other_roots_server_is_a_stray_and_it_stops() {
    use super::strays::{stop, strays, StrayStop, StrayTarget};
    let binary = sidecar_binary();
    let mine = short_root();
    let other = short_root();
    let _reap_mine = ReapSidecarsAt::new(mine.path());
    let _reap_other = ReapSidecarsAt::new(other.path());
    let launch =
        |root: &Path| launch_detached_with_env(&binary, root, None, None, &[tie_to_this_process()]);
    let state = SessionServerState::at(Some(mine.path().to_path_buf()));
    let attached = state
        .connect_or_launch(mine.path(), Arc::new(RecordingSink::default()), launch)
        .expect("this app's server");
    let other_state = SessionServerState::at(Some(other.path().to_path_buf()));
    let other_client = other_state
        .connect_or_launch(other.path(), Arc::new(RecordingSink::default()), launch)
        .expect("the other root's server");
    other_client
        .spawn(sh("sleep 600"), registration("left-behind", None, false))
        .expect("a session on the other server");
    let other_pid = other_client.pid();
    drop(other_client);
    drop(other_state);

    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap();
    #[cfg(target_os = "macos")]
    {
        use super::strays::process_exe;

        struct HeldCaffeinateShell {
            child: std::cell::RefCell<std::process::Child>,
            input: Option<std::process::ChildStdin>,
        }
        impl Drop for HeldCaffeinateShell {
            fn drop(&mut self) {
                drop(self.input.take());
                // Kill/wait also runs on assertion panic; a fork retaining the
                // pipe writer cannot leave this owned shell waiting for EOF.
                let child = self.child.get_mut();
                let _ = child.kill();
                let _ = child.wait();
            }
        }

        // The sidecar double-forks. Keep caffeinate monitoring an owned shell
        // while its detached daemon serves the unique root independently.
        let wrapped_root = short_root();
        let _reap_wrapped = ReapSidecarsAt::new(wrapped_root.path());
        let mut wrapped = HeldCaffeinateShell {
            child: std::cell::RefCell::new(
                std::process::Command::new("/usr/bin/caffeinate")
                    .args([
                        "-i",
                        "/bin/bash",
                        "-c",
                        "\"$@\" && { read -r _ || :; }",
                        "bash",
                    ])
                    .arg(&binary)
                    .arg("--root")
                    .arg(wrapped_root.path())
                    .env(tie_to_this_process().0, tie_to_this_process().1)
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .expect("caffeinate runs the held shell"),
            ),
            input: None,
        };
        wrapped.input = wrapped.child.get_mut().stdin.take();
        assert!(
            wrapped.input.is_some(),
            "the parent holds the shell's stdin pipe"
        );
        let shell_pid = wrapped.child.borrow().id();
        let shell_exe = canonical(Path::new("/bin/bash"));
        let server_exe = canonical(&binary);
        let monitor_exe = canonical(Path::new("/usr/bin/caffeinate"));
        let ready_roles = std::cell::RefCell::new(None);
        let last_readiness = std::cell::RefCell::new(String::new());
        assert!(
            wait_until(|| {
                let identity = read_server_identity(wrapped_root.path(), |_| true);
                let actual_shell_exe = process_exe(shell_pid);
                let daemon_exe = identity
                    .as_ref()
                    .ok()
                    .and_then(|identity| identity.as_ref())
                    .and_then(|identity| process_exe(identity.pid));
                // The private root in argv establishes fixture ownership;
                // the kernel executable distinguishes the actual monitor.
                let monitors: Vec<_> = sidecar_pids_rooted_at(wrapped_root.path())
                    .into_iter()
                    .filter_map(|pid| process_exe(pid).map(|exe| (pid, exe)))
                    .filter(|(_, exe)| exe == &monitor_exe)
                    .collect();
                *last_readiness.borrow_mut() = format!(
                    "root={:?}, shell_pid={shell_pid}, shell_exe={actual_shell_exe:?}, \
                     identity={identity:?}, daemon_exe={daemon_exe:?}, monitors={monitors:?}",
                    wrapped_root.path()
                );
                let Ok(Some(identity)) = identity else {
                    return false;
                };
                if actual_shell_exe.as_ref() != Some(&shell_exe)
                    // The daemon may read as its `activity-helper` hard link.
                    || !daemon_exe
                        .as_deref()
                        .is_some_and(|exe| super::strays::same_file(exe, &server_exe))
                    || monitors.len() != 1
                    || identity.pid == shell_pid
                    || monitors[0].0 == shell_pid
                    || monitors[0].0 == identity.pid
                {
                    return false;
                }
                *ready_roles.borrow_mut() = Some((identity, monitors[0].0));
                true
            }),
            "the detached server must complete its handshake while the owned shell and monitor remain live: {}",
            last_readiness.borrow()
        );
        let (wrapped_identity, monitor_pid) = ready_roles.into_inner().unwrap();
        eprintln!("AGR19 caffeinate roles: {}", last_readiness.borrow());
        let listed = strays(Some(attached.pid()), Some(&binary), Some(mine.path()));
        for (pid, exe) in [(shell_pid, &shell_exe), (monitor_pid, &monitor_exe)] {
            assert_eq!(
                process_exe(pid).as_ref(),
                Some(exe),
                "the owned shell and monitor must remain live through census: {}",
                last_readiness.borrow()
            );
            assert!(
                listed.iter().all(|stray| stray.pid != pid),
                "the shell and caffeinate monitor are never strays: {}; listed={listed:?}",
                last_readiness.borrow()
            );
        }
        let wrapped_root_name = canonical(wrapped_root.path())
            .to_string_lossy()
            .into_owned();
        assert!(
            listed.iter().any(|stray| {
                stray.root == wrapped_root_name && stray.pid == wrapped_identity.pid
            }),
            "the exact handshake daemon is a stray: {}; listed={listed:?}",
            last_readiness.borrow()
        );

        let input: &mut std::process::ChildStdin = wrapped
            .input
            .as_mut()
            .expect("the parent still holds the shell's stdin pipe");
        writeln!(input).unwrap_or_else(|error| {
            panic!(
                "release the owned shell with a newline: {error}; {}",
                last_readiness.borrow()
            )
        });
        drop(wrapped.input.take());
        let shell_status = std::cell::Cell::new(None);
        assert!(
            wait_until(|| {
                let status = wrapped
                    .child
                    .borrow_mut()
                    .try_wait()
                    .expect("poll owned shell");
                shell_status.set(status);
                status.is_some()
            }),
            "the owned shell must exit after its release newline: {}",
            last_readiness.borrow()
        );
        let shell_status = shell_status.get().expect("the owned shell has exited");
        assert!(
            shell_status.success(),
            "the owned shell must exit successfully, got {shell_status}: {}",
            last_readiness.borrow()
        );
        assert!(
            wait_until(|| !alive(monitor_pid)),
            "the owned caffeinate monitor must exit after its shell: {}",
            last_readiness.borrow()
        );
    }

    let listed = strays(Some(attached.pid()), Some(&binary), Some(mine.path()));
    let mine_root = canonical(mine.path()).to_string_lossy().into_owned();
    assert!(
        listed.iter().all(|stray| stray.root != mine_root),
        "the attached server is never a stray: {listed:?}"
    );
    let stray = listed
        .iter()
        .find(|stray| stray.pid == other_pid)
        .unwrap_or_else(|| panic!("the other root's server is listed: {listed:?}"));
    assert_eq!(stray.root, canonical(other.path()).to_string_lossy());
    assert_eq!(stray.sessions, Some(1));
    assert!(!stray.started.is_empty());

    // A stale confirmation (another start time) signals nothing.
    let stale = StrayTarget {
        pid: stray.pid,
        started: "Thu Jan  1 00:00:00 1970".into(),
        root: stray.root.clone(),
    };
    assert_eq!(
        stop(&stale, Some(attached.pid()), Some(mine.path())),
        StrayStop::Gone
    );
    assert!(alive(other_pid));
    let confirmed = StrayTarget {
        pid: stray.pid,
        started: stray.started.clone(),
        root: stray.root.clone(),
    };
    assert!(matches!(
        stop(&confirmed, Some(attached.pid()), Some(mine.path())),
        StrayStop::Stopped { .. }
    ));
    assert!(!alive(other_pid));
    assert!(alive(attached.pid()), "this app's server keeps running");
    // While the app is not attached (reconnecting, or an incompatible
    // server), its own server — the lock holder — is still never a stray,
    // and never stopped here.
    let unattached = strays(None, Some(&binary), Some(mine.path()));
    assert!(
        unattached.iter().all(|stray| stray.pid != attached.pid()),
        "{unattached:?}"
    );
    let own = server_processes_for_test(attached.pid());
    assert_eq!(
        stop(
            &StrayTarget {
                pid: attached.pid(),
                started: own,
                root: mine_root.clone(),
            },
            None,
            Some(mine.path())
        ),
        StrayStop::Attached
    );
    assert!(alive(attached.pid()));
}

/// The start time `ps` gives `pid`, as the stray list reads it.
fn server_processes_for_test(pid: u32) -> String {
    super::strays::server_processes(unsafe { libc::getuid() })
        .into_iter()
        .find(|process| process.pid == pid)
        .map(|process| process.started)
        .unwrap_or_default()
}

#[test]
fn a_local_activity_and_inbox_host_without_spend_holds_refuses_capped_launches() {
    let mut hello = mock_hello("cap-compat".into());
    hello
        .capabilities
        .extend([Capability::ActivityV2, Capability::InboxWaitV1]);
    hello
        .capabilities
        .retain(|c| c != &Capability::InboxWakeHoldsV1);
    assert!(hello.supports(&Capability::ActivityV2));
    assert!(hello.supports(&Capability::InboxWaitV1));
    let why = super::require_spend_hold_capability(&hello).unwrap_err();
    assert!(why.contains("enforcement needs a terminal-server update"));
    assert!(why.contains("Restart terminal server"));
    hello.capabilities.push(Capability::InboxWakeHoldsV1);
    assert!(super::require_spend_hold_capability(&hello).is_ok());
}

#[test]
fn an_old_daemon_cannot_confirm_worktree_safety_and_receives_no_query() {
    let root = short_root();
    let mock = mock_sidecar(root.path(), 1, |_, _, _| {});
    let client = SessionServerClient::connect(
        root.path(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let error = client.worktree_usage("/source").unwrap_err().to_string();
    assert!(error.contains("terminal server too old"));
    assert!(error.contains("restart it"));
    drop(client);
    assert!(mock.join().unwrap().is_empty());
}

#[test]
fn a_current_terminal_server_positively_confirms_an_empty_worktree() {
    let sidecar = TestServer::start();
    let client = SessionServerClient::connect(
        sidecar.root(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let source = tempfile::tempdir().unwrap();
    let usage = client
        .worktree_usage(source.path().to_str().unwrap())
        .unwrap();
    assert!(usage.entries.is_empty());
    assert!(usage.opening.is_empty());
}
