// SPDX-License-Identifier: MIT
//! Real fault oracles. Invoke only through the C2-owned fault producer.
use super::rm4f_support::{OracleTrace, OwnedManagerHarness, RecordingPresentation};
use super::*;
use crate::commands::pty_host::{StreamHost, StreamItem};
use crate::terminal::session_server::{NamespacedTerminalEvent, NamespacedTerminalEventSink};
use codesense_pty_core::domain::{
    PortUnavailable, PtySessionId, TerminalDomainEvent, TerminalEventSink, ViewportId,
};
use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};
use codesense_pty_core::term_state::StreamId;
use codesense_pty_core::{layout::ServerIdentity, TerminalHost};
use codesense_session_server::testing::{registration, sh, sh_spec, wait_until};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::time::Instant;

const SESSION: &str = "rm4f-retained-session";

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing owned input {name}"))
}

struct Checkpoints {
    request: std::fs::File,
    response: std::fs::File,
    run: String,
    scenario: &'static str,
    sequence: u64,
}

impl Checkpoints {
    fn new(scenario: &'static str) -> Self {
        let open = |name| {
            let path = required(name);
            assert!(std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_fifo());
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
                .unwrap()
        };
        Self {
            request: open("RM4F_FAULT_REQUEST"),
            response: open("RM4F_FAULT_RESPONSE"),
            run: required("RM4F_RUN_ID"),
            scenario,
            sequence: 0,
        }
    }

    fn exchange(&mut self, command: &str, payload: Value, expected: &str) -> Value {
        self.sequence += 1;
        let message = json!({"version": 1, "run_id": self.run, "scenario_id": self.scenario,
            "sequence": self.sequence, "command": command, "payload": payload});
        let mut wire = serde_json::to_vec(&message).unwrap();
        wire.push(b'\n');
        assert!(wire.len() < 4096, "checkpoint exceeds atomic FIFO write");
        self.request.write_all(&wire).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut bytes = Vec::new();
        loop {
            assert!(
                Instant::now() < deadline,
                "fault checkpoint deadline: {command}"
            );
            let mut byte = [0];
            match self.response.read(&mut byte) {
                Ok(1) if byte[0] == b'\n' => break,
                Ok(1) => bytes.push(byte[0]),
                Ok(_) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(error) => panic!("checkpoint read: {error}"),
            }
            assert!(bytes.len() < 16384, "oversized checkpoint response");
        }
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response.as_object().unwrap().len(), 6);
        assert_eq!(response["version"], 1);
        assert_eq!(response["run_id"], self.run);
        assert_eq!(response["scenario_id"], self.scenario);
        assert_eq!(response["sequence"], self.sequence);
        assert_eq!(response["command"], expected);
        response["payload"].clone()
    }
}

struct LocalSink(Arc<RecordingPresentation>);
impl TerminalEventSink for LocalSink {
    fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        if let TerminalDomainEvent::StreamOutput { stream, data, seq } = event {
            self.0.streams.deliver_output(
                StreamHost::InProcess,
                stream,
                data,
                seq,
                &mut |local, data, _| {
                    lock(&self.0.output).push((local, data.into()));
                    true
                },
            );
        }
        Ok(())
    }
}

struct LocalControl {
    host: TerminalHost,
    pid: u32,
    cleanup: LocalCleanup,
}
type LocalCleanup = Arc<Mutex<Option<Result<(), String>>>>;

fn record_local_cleanup(outcome: &LocalCleanup, cleanup: impl FnOnce() -> Result<(), String>) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup))
        .unwrap_or_else(|_| Err("local cleanup panicked".into()));
    *outcome.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(result);
}

fn local_cleanup_result(outcome: &LocalCleanup) -> Result<(), String> {
    outcome
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
        .unwrap_or_else(|| Err("local cleanup did not complete".into()))
}

impl Drop for LocalControl {
    fn drop(&mut self) {
        record_local_cleanup(&self.cleanup, || {
            self.host
                .kill(&PtySessionId::new(SESSION))
                .map_err(|error| format!("local kill: {error:?}"))?;
            if !wait_until(|| !codesense_session_server::testing::alive(self.pid)) {
                return Err("local PTY not reaped before deadline".into());
            }
            Ok(())
        });
    }
}

fn isolated_output(before: &Value, after: &Value) -> bool {
    let labels = ["A_", "B_", "LOCAL_"];
    [before, after].into_iter().all(|snapshot| {
        snapshot["output"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .all(|(index, output)| {
                labels.iter().enumerate().all(|(foreign, label)| {
                    index == foreign || !output.as_str().unwrap().contains(label)
                })
            })
    })
}

#[test]
fn local_cleanup_failure_during_unwind_is_recorded_without_second_panic() {
    struct Guard(LocalCleanup, bool);
    impl Drop for Guard {
        fn drop(&mut self) {
            record_local_cleanup(&self.0, || {
                if self.1 {
                    panic!("injected cleanup panic");
                }
                Err("injected kill/reap failure".into())
            });
        }
    }
    for panics in [false, true] {
        let outcome = LocalCleanup::default();
        assert!(local_cleanup_result(&outcome).is_err());
        let caught = std::panic::catch_unwind(|| {
            let _guard = Guard(outcome.clone(), panics);
            panic!("original product assertion");
        });
        assert!(caught.is_err());
        assert!(local_cleanup_result(&outcome).is_err());
    }
    let successful = LocalCleanup::default();
    record_local_cleanup(&successful, || Ok(()));
    assert!(local_cleanup_result(&successful).is_ok());
}

#[test]
fn output_isolation_checks_every_direction_in_both_phases() {
    let baseline = json!({"output": ["A_BEFORE", "B_BEFORE", "LOCAL_BEFORE"]});
    assert!(isolated_output(&baseline, &baseline));
    for phase in 0..2 {
        for destination in 0..3 {
            for (source, label) in ["A_", "B_", "LOCAL_"].iter().enumerate() {
                if source == destination {
                    continue;
                }
                let mut snapshots = [baseline.clone(), baseline.clone()];
                snapshots[phase]["output"][destination] = json!(format!(
                    "{} {label}FOREIGN",
                    snapshots[phase]["output"][destination].as_str().unwrap()
                ));
                assert!(!isolated_output(&snapshots[0], &snapshots[1]));
            }
        }
    }
}

fn output(presentation: &RecordingPresentation, local: u64) -> String {
    lock(&presentation.output)
        .iter()
        .filter(|(id, _)| *id == local)
        .map(|(_, data)| data.as_str())
        .collect()
}

fn script(host: &str) -> String {
    format!("stty -echo; while read line; do printf '{host}_%s\\n' \"$line\"; done")
}

#[derive(Default)]
struct PayloadLedger(Mutex<BTreeMap<&'static str, Vec<String>>>);
impl PayloadLedger {
    fn write(
        &self,
        host: &'static str,
        domain: &dyn TerminalDomain,
        session: &PtySessionId,
        data: &str,
    ) {
        domain.write(session, data.as_bytes()).unwrap();
        lock(&self.0).entry(host).or_default().push(data.to_owned());
    }

    fn snapshot(&self) -> [Vec<String>; 3] {
        let observed = lock(&self.0);
        ["A", "B", "LOCAL"].map(|host| observed.get(host).cloned().unwrap_or_default())
    }
}

fn proof(harness: &OwnedManagerHarness, index: usize) -> Value {
    let proofs = harness.admitted_proofs(&harness.machines.ids[index]);
    serde_json::to_value(proofs.last().expect("missing actual admitted proof")).unwrap()
}

fn run_fault(harness: &OwnedManagerHarness, scenario: &'static str, cleanup: LocalCleanup) {
    let daemon = scenario == "RM4F-02";
    let runtime = tokio::runtime::Handle::current();
    for index in 0..2 {
        runtime.block_on(harness.connect(index)).unwrap();
    }
    let a = harness
        .manager
        .connected_backend(&harness.machines.ids[0])
        .unwrap();
    let b = harness
        .manager
        .connected_backend(&harness.machines.ids[1])
        .unwrap();
    let clients = [a.client(), b.client()];
    let payloads = PayloadLedger::default();
    let session = PtySessionId::new(SESSION);
    let viewport = ViewportId::new("rm4f-retained-viewport");
    let mut pids = Vec::new();
    let mut locals = Vec::new();
    let mut wire_streams = Vec::new();
    for (client, label) in clients.iter().zip(["A", "B"]) {
        let mut intent = sh(&script(label));
        intent.shell = Some("/bin/bash".into());
        intent.cwd = "/tmp".into();
        pids.push(
            client
                .spawn(intent, registration(SESSION, Some("rm4f"), false))
                .unwrap()
                .unwrap(),
        );
        let subscription = client.subscribe(&session, &viewport).unwrap();
        wire_streams.push(subscription.stream_id);
        locals.push(harness.presentation.streams.attach(
            StreamHost::remote_namespace(client.namespace()),
            &session,
            &viewport,
            &subscription,
        ));
    }
    let host = TerminalHost::new(
        ServerIdentity::ephemeral("rm4f-local"),
        Arc::new(LocalSink(harness.presentation.clone())),
        runtime.clone(),
    );
    // The parent owns this exact builtin-only shell via its pre-spawn nonce.
    // LOCAL_ distinguishes process output from PTY echo without a stty child.
    let mut local_spec = sh_spec(SESSION, "");
    local_spec.command = portable_pty::CommandBuilder::new("/bin/sh");
    local_spec.command.args([
        "-c",
        "while IFS= read -r line; do printf 'LOCAL_%s\\n' \"$line\"; done",
        &required("RM4F_NATIVE_CONTROL_NONCE"),
    ]);
    local_spec.command.cwd("/tmp");
    let child = runtime.block_on(LocalPtyRunner.spawn(local_spec)).unwrap();
    let pid = child.pid().unwrap();
    host.register(child, registration(SESSION, Some("rm4f"), false))
        .unwrap();
    let local = LocalControl { host, pid, cleanup };
    let subscription = local.host.subscribe(&session, &viewport).unwrap();
    locals.push(harness.presentation.streams.attach(
        StreamHost::InProcess,
        &session,
        &viewport,
        &subscription,
    ));
    let local_identity = local.host.identity().unwrap();
    let sentinel = |phase: &str, a_client: &SessionServerClient| {
        for (domain, label, stream) in [
            (a_client as &dyn TerminalDomain, "A", locals[0]),
            (clients[1].as_ref() as &dyn TerminalDomain, "B", locals[1]),
            (&local.host as &dyn TerminalDomain, "LOCAL", locals[2]),
        ] {
            payloads.write(label, domain, &session, &format!("{phase}\r"));
            assert!(
                wait_until(
                    || output(&harness.presentation, stream).contains(&format!("{label}_{phase}"))
                ),
                "missing {label} {phase} output"
            );
        }
    };
    sentinel("BEFORE", &clients[0]);
    let before_sessions = [
        clients[0].live_session_ids().unwrap(),
        clients[1].live_session_ids().unwrap(),
        local.host.live_session_ids().unwrap(),
    ];
    assert!(
        before_sessions
            .iter()
            .all(|sessions| sessions.contains(&session)),
        "initial census missing controlled session"
    );
    let before = json!({"a": proof(harness, 0), "b": proof(harness, 1),
        "namespaces": [clients[0].namespace(), clients[1].namespace()],
        "session": SESSION, "session_pids": pids, "mappings": locals,
        "live_sessions": before_sessions.iter().map(|set| set.iter().map(|id| id.as_str()).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "input_commands": payloads.snapshot(),
        "local": {"machine_id": "local", "server_id": local_identity.server.id, "epoch": local_identity.epoch, "pid": local.pid},
        "output": locals.iter().map(|id| output(&harness.presentation, *id)).collect::<Vec<_>>()});
    let old_ledger = lock(&harness.manager.core).machines[&harness.machines.ids[0]]
        .remote_ledger
        .clone();
    let mut checkpoints = Checkpoints::new(scenario);
    let cut = checkpoints.exchange(
        "READY",
        json!({"proof": before["a"], "local_pid": local.pid,
            "local_nonce": required("RM4F_NATIVE_CONTROL_NONCE")}),
        if daemon { "DAEMON_EXITED" } else { "CUT" },
    );
    let loss = wait_until(|| {
        harness
            .manager
            .connected_backend(&harness.machines.ids[0])
            .is_err()
    });
    // B/local must answer while A is cut, not just before and after reconnect.
    payloads.write("B", clients[1].as_ref(), &session, "DURING\r");
    payloads.write("LOCAL", &local.host, &session, "DURING\r");
    assert!(wait_until(
        || output(&harness.presentation, locals[1]).contains("B_DURING")
    ));
    assert!(wait_until(
        || output(&harness.presentation, locals[2]).contains("LOCAL_DURING")
    ));
    let restored = checkpoints.exchange(
        "LOSS_OBSERVED",
        json!({"transport_loss": loss}),
        if daemon {
            "RECONNECT_READY"
        } else {
            "RESTORED"
        },
    );
    if daemon
        && std::env::var("RM4F_FAULT_MUTANT").as_deref()
            == Ok("rm4f_reuse_old_epoch_after_daemon_exit")
    {
        *lock(&harness.manager.rm4f_reuse_old_epoch_after_daemon_exit) =
            Some(harness.machines.ids[0].clone());
    }
    if loss {
        runtime.block_on(harness.connect(0)).unwrap();
    }
    let replacement = harness
        .manager
        .connected_backend(&harness.machines.ids[0])
        .unwrap();
    if daemon {
        daemon_finish(DaemonRun {
            harness,
            clients: &clients,
            locals: &locals,
            local: &local,
            before,
            cut,
            restored,
            replacement,
            checkpoints,
            old_ledger,
            old_wire: wire_streams[0],
            payloads: &payloads,
        });
        return;
    }
    let new_client = replacement.client();
    let old_namespace = clients[0].namespace().clone();
    let mut failed_resume = Vec::new();
    if loss {
        // Headless admission deliberately uses the production resumption primitive
        // explicitly; this is not evidence of AppHandle automatic resubscription.
        failed_resume = harness.presentation.streams.resume_from_to(
            StreamHost::remote_namespace(&old_namespace),
            StreamHost::remote_namespace(new_client.namespace()),
            || harness.manager.revalidate_backend(&replacement).is_ok(),
            |session, viewport| new_client.subscribe(session, viewport),
            |stream| {
                new_client.unsubscribe(stream).unwrap();
            },
            &mut |local, item| {
                if let StreamItem::Output { data, .. } = item {
                    lock(&harness.presentation.output).push((local, data.into()));
                }
                true
            },
        );
    }
    sentinel("AFTER", &new_client);
    let b_after = harness
        .manager
        .connected_backend(&harness.machines.ids[1])
        .unwrap();
    let session_sets = [
        new_client.live_session_ids().unwrap(),
        clients[1].live_session_ids().unwrap(),
        local.host.live_session_ids().unwrap(),
    ];
    let measured_mappings: Vec<Vec<u64>> = [
        StreamHost::remote_namespace(new_client.namespace()),
        StreamHost::remote_namespace(b_after.client().namespace()),
        StreamHost::InProcess,
    ]
    .into_iter()
    .map(|host| {
        harness
            .presentation
            .streams
            .attached_on(host)
            .into_iter()
            .filter(|(_, id, view)| id == &session && view == &viewport)
            .map(|(id, _, _)| id)
            .collect()
    })
    .collect();
    let local_after = local.host.identity().unwrap();
    let retirement_events = lock(&harness.presentation.events)
        .iter()
        .filter(|event| {
            matches!(
                event.event,
                TerminalDomainEvent::Exit { .. } | TerminalDomainEvent::SessionRemoved { .. }
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let after = json!({"a": proof(harness, 0), "b": proof(harness, 1),
        "namespaces": [new_client.namespace(), b_after.client().namespace()],
        "session": SESSION, "mappings": measured_mappings.iter().flatten().collect::<Vec<_>>(),
        "input_commands": payloads.snapshot(),
        "local": {"machine_id": "local", "server_id": local_after.server.id, "epoch": local_after.epoch, "pid": local.pid},
        "retirement_events": retirement_events,
        "live_sessions": session_sets.iter().map(|set| set.iter().map(|id| id.as_str()).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "output": locals.iter().map(|id| output(&harness.presentation, *id)).collect::<Vec<_>>()});
    let checks = json!({"transport_loss": loss, "proxy_absent": cut["proxy_absent"] == true,
        "same_daemon": before["a"] == after["a"], "same_b": before["b"] == after["b"] && Arc::ptr_eq(&clients[1], &b_after.client()) && measured_mappings[1] == [locals[1]],
        "same_local": local_after == local_identity && measured_mappings[2] == [locals[2]],
        "retained_sessions": session_sets.iter().all(|set| set.contains(&session)),
        "new_client": !Arc::ptr_eq(&clients[0], &new_client),
        "headless_resubscription": loss && failed_resume.is_empty() && harness.presentation.streams.attached_on(StreamHost::remote_namespace(new_client.namespace())).iter().any(|(id, _, _)| *id == locals[0]),
        "no_cross_output": isolated_output(&before, &after),
        "no_retirement": retirement_events.is_empty()});
    checkpoints.exchange("DONE", json!({}), "JOINED");
    let measurement = json!({"version": 1, "run_id": required("RM4F_RUN_ID"), "scenario_id": "RM4F-01",
        "mutant": std::env::var("RM4F_FAULT_MUTANT").ok(),
        "observations": {"before": before, "cut": cut, "restored": restored, "after": after,
            "payload_scope": "observed application sentinel bytes; not encrypted SSH traffic"}, "outcome_checks": checks});
    write_measurement(&measurement);
}

struct DaemonRun<'a> {
    harness: &'a OwnedManagerHarness,
    clients: &'a [Arc<SessionServerClient>; 2],
    locals: &'a [u64],
    local: &'a LocalControl,
    before: Value,
    cut: Value,
    restored: Value,
    replacement: ConnectedBackendSnapshot,
    checkpoints: Checkpoints,
    old_ledger: RemoteSessionLedger,
    old_wire: StreamId,
    payloads: &'a PayloadLedger,
}

fn daemon_finish(mut run: DaemonRun<'_>) {
    let harness = run.harness;
    let client = run.replacement.client();
    let old = run.clients[0].namespace();
    let session = PtySessionId::new(SESSION);
    let trace = lock(&harness.presentation.trace).clone();
    let exits: Vec<usize> = trace
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            OracleTrace::Terminal { event }
                if event.namespace == *old
                    && matches!(&event.event,
            TerminalDomainEvent::Exit { session, .. } if session.as_str() == SESSION) =>
            {
                Some(index)
            }
            _ => None,
        })
        .collect();
    let removed: Vec<usize> = trace
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            OracleTrace::Terminal { event }
                if event.namespace == *old
                    && matches!(&event.event,
            TerminalDomainEvent::SessionRemoved { session, .. } if session.as_str() == SESSION) =>
            {
                Some(index)
            }
            _ => None,
        })
        .collect();
    let publication = trace.iter().position(|item| {
        matches!(item,
        OracleTrace::Publication { machine_id, namespace: Some(namespace) }
        if machine_id == &harness.machines.ids[0] && namespace == client.namespace())
    });
    let first_candidate_event = trace.iter().position(|item| {
        matches!(item,
        OracleTrace::Terminal { event } if event.namespace == *client.namespace())
    });
    let retirement_events = lock(&harness.presentation.events)
        .iter()
        .filter(|event| {
            event.namespace == *old
                && matches!(
                    event.event,
                    TerminalDomainEvent::Exit { .. } | TerminalDomainEvent::SessionRemoved { .. }
                )
        })
        .cloned()
        .collect::<Vec<_>>();
    // Observe without draining: repeated production retirement must see the
    // original state, including a mutant's still-held sessions/generation.
    let ledger_remaining = run.old_ledger.rm4f_snapshot();
    let old_mapping = harness
        .presentation
        .streams
        .attached_on(StreamHost::remote_namespace(old));
    let before_late = output(&harness.presentation, run.locals[0]);
    harness
        .presentation
        .emit(NamespacedTerminalEvent {
            namespace: old.clone(),
            event: TerminalDomainEvent::StreamOutput {
                stream: run.old_wire,
                data: "LATE_OLD_A".into(),
                seq: 1_000_000,
            },
        })
        .unwrap();
    let after_late = output(&harness.presentation, run.locals[0]);
    let repeated = crate::commands::pty_backend::retire_remote_lifetime_core(
        &harness.presentation.streams,
        harness.presentation.as_ref(),
        old,
        run.old_ledger.clone(),
        || true,
    );
    let retirement_count_after_repeat = lock(&harness.presentation.events)
        .iter()
        .filter(|event| {
            event.namespace == *old
                && matches!(
                    event.event,
                    TerminalDomainEvent::Exit { .. } | TerminalDomainEvent::SessionRemoved { .. }
                )
        })
        .count();
    let old_session_present = client.has_session(&session).unwrap();
    let fresh = PtySessionId::new("rm4f-fresh-after-daemon");
    let mut intent = sh(&script("A"));
    intent.shell = Some("/bin/bash".into());
    intent.cwd = "/tmp".into();
    let fresh_pid = client
        .spawn(intent, registration(fresh.as_str(), Some("rm4f"), false))
        .unwrap()
        .unwrap();
    let viewport = ViewportId::new("rm4f-fresh-viewport");
    let subscribed = client.subscribe(&fresh, &viewport).unwrap();
    let fresh_local = harness.presentation.streams.attach(
        StreamHost::remote_namespace(client.namespace()),
        &fresh,
        &viewport,
        &subscribed,
    );
    run.payloads.write("A", client.as_ref(), &fresh, "AFTER\r");
    run.payloads
        .write("B", run.clients[1].as_ref(), &session, "AFTER\r");
    run.payloads
        .write("LOCAL", &run.local.host, &session, "AFTER\r");
    for (stream, sentinel) in [
        (fresh_local, "A_AFTER"),
        (run.locals[1], "B_AFTER"),
        (run.locals[2], "LOCAL_AFTER"),
    ] {
        assert!(
            wait_until(|| output(&harness.presentation, stream).contains(sentinel)),
            "missing fresh/control output {sentinel}"
        );
    }
    let b = harness
        .manager
        .connected_backend(&harness.machines.ids[1])
        .unwrap();
    let local_identity = run.local.host.identity().unwrap();
    let mappings: Vec<Vec<u64>> = [
        StreamHost::remote_namespace(client.namespace()),
        StreamHost::remote_namespace(b.client().namespace()),
        StreamHost::InProcess,
    ]
    .into_iter()
    .map(|host| {
        harness
            .presentation
            .streams
            .attached_on(host)
            .into_iter()
            .map(|(id, _, _)| id)
            .collect()
    })
    .collect();
    let sessions = [
        client.live_session_ids().unwrap(),
        b.client().live_session_ids().unwrap(),
        run.local.host.live_session_ids().unwrap(),
    ];
    let after = json!({"a": proof(harness, 0), "b": proof(harness, 1),
        "namespaces": [client.namespace(), b.client().namespace()], "session": SESSION,
        "mappings": mappings.iter().flatten().collect::<Vec<_>>(),
        "input_commands": run.payloads.snapshot(),
        "local": {"machine_id": "local", "server_id": local_identity.server.id, "epoch": local_identity.epoch, "pid": run.local.pid},
        "live_sessions": sessions.iter().map(|set| set.iter().map(|id| id.as_str()).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "output": [output(&harness.presentation, fresh_local), output(&harness.presentation, run.locals[1]), output(&harness.presentation, run.locals[2])],
        "retirement_events": retirement_events, "old_session_present": old_session_present,
        "fresh": {"session": fresh.as_str(), "pid": fresh_pid, "mapping": fresh_local}});
    let joined = run
        .checkpoints
        .exchange("DONE", json!({"proof": after["a"]}), "JOINED");
    run.restored["daemon"] = joined["daemon"].clone();
    let ordered = exits.len() == 1
        && removed.len() == 1
        && exits[0] < removed[0]
        && publication.is_some_and(|index| removed[0] < index)
        && first_candidate_event.is_none_or(|index| removed[0] < index);
    let checks = json!({"daemon_exited": run.cut["signal_result"]["pidfd_exit_observed"] == true,
        "transport_loss": run.restored["loss_observed"] == true,
        "same_server": run.before["a"]["server_id"] == after["a"]["server_id"],
        "new_epoch": run.before["a"]["epoch"] != after["a"]["epoch"],
        "new_instance": run.cut["daemon"]["pid"] != run.restored["daemon"]["pid"] || run.cut["daemon"]["starttime"] != run.restored["daemon"]["starttime"],
        "old_session_removed": !old_session_present, "retired_once": exits.len() == 1 && removed.len() == 1,
        "retired_before_publication": ordered, "ledger_drained": ledger_remaining.is_empty(),
        "idempotent_retirement": !repeated && retirement_count_after_repeat == retirement_events.len(),
        "late_output_suppressed": before_late == after_late && old_mapping.is_empty(),
        "same_b": run.before["b"] == after["b"] && Arc::ptr_eq(&run.clients[1], &b.client()) && mappings[1] == [run.locals[1]] && sessions[1].contains(&session),
        "same_local": run.before["local"] == after["local"] && mappings[2] == [run.locals[2]] && sessions[2].contains(&session),
        "fresh_request": sessions[0].contains(&fresh) && output(&harness.presentation, fresh_local).contains("A_AFTER"),
        "no_cross_output": isolated_output(&run.before, &after)});
    write_measurement(
        &json!({"version": 1, "run_id": required("RM4F_RUN_ID"), "scenario_id": "RM4F-02",
        "mutant": std::env::var("RM4F_FAULT_MUTANT").ok(), "observations": {"before": run.before,
            "cut": run.cut, "restored": run.restored, "after": after,
            "retirement": {"trace": trace, "exit_indices": exits, "removed_indices": removed, "publication_index": publication,
                "first_candidate_event_index": first_candidate_event, "ledger_remaining": ledger_remaining,
                "old_mappings": old_mapping, "late_output_before": before_late, "late_output_after": after_late,
                "repeat_applied": repeated, "retirement_count_after_repeat": retirement_count_after_repeat},
            "payload_scope": "observed application sentinel bytes; not encrypted SSH traffic"}, "outcome_checks": checks}),
    );
}

fn write_measurement(measurement: &Value) {
    let mut measurement = measurement.clone();
    let probe: Value = serde_json::from_slice(
        &std::fs::read(
            std::path::Path::new(&required("RM4F_FAULT_ROOT")).join("native-probe.json"),
        )
        .unwrap(),
    )
    .unwrap();
    measurement["observations"]["native_control"] = json!({
        "pid": measurement["observations"]["before"]["local"]["pid"],
        "nonce": required("RM4F_NATIVE_CONTROL_NONCE"), "probe": probe});
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(required("RM4F_RAW_MEASUREMENT"))
        .unwrap();
    serde_json::to_writer_pretty(&mut file, &measurement).unwrap();
    file.flush().unwrap();
    file.sync_all().unwrap();
    assert!(
        measurement["outcome_checks"]
            .as_object()
            .unwrap()
            .values()
            .all(|value| value == true),
        "measured PRODUCT_FAILURE: {}",
        measurement["outcome_checks"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires C2-owned A/B fresh slots and joined fault controller"]
async fn rm4f_01_network_cut() {
    owned_case("RM4F-01").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires C2-owned A/B fresh slots and joined exact pidfd fault controller"]
async fn rm4f_02_daemon_exit() {
    owned_case("RM4F-02").await;
}

async fn owned_case(scenario: &'static str) {
    use futures_util::FutureExt;
    let harness = OwnedManagerHarness::supplied().await.unwrap();
    let cleanup = LocalCleanup::default();
    let work = harness
        .with_cleanup(async {
            std::panic::AssertUnwindSafe(async {
                tokio::task::block_in_place(|| run_fault(&harness, scenario, cleanup.clone()));
            })
            .catch_unwind()
            .await
        })
        .await;
    tokio::task::block_in_place(|| {
        local_cleanup_result(&cleanup).expect("HARNESS_FAILURE: local control cleanup");
        complete_measurement(scenario);
    });
    if let Err(panic) = work {
        std::panic::resume_unwind(panic);
    }
}

fn complete_measurement(scenario: &'static str) {
    use sha2::{Digest, Sha256};
    // Only reaching this boundary proves with_cleanup returned after its drain.
    // An assertion is rethrown afterwards so a mutant remains a failing test.
    let mut completion = json!({"version": 1, "run_id": required("RM4F_RUN_ID"),
        "scenario_id": scenario, "owner_token": required("RM4F_OWNER_TOKEN"),
        "test_pid": std::process::id(), "manager_drained": true, "ssh_owners_reaped": true});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(required("RM4F_DRAIN_COMPLETION"))
        .unwrap();
    serde_json::to_writer(&mut file, &completion).unwrap();
    file.flush().unwrap();
    file.sync_all().unwrap();
    let raw_path = std::path::PathBuf::from(required("RM4F_RAW_MEASUREMENT"));
    if raw_path.exists() {
        assert!(raw_path.symlink_metadata().unwrap().file_type().is_file());
        assert_eq!(raw_path.file_name().unwrap(), "measurement.raw.json");
        let raw = std::fs::read(raw_path).unwrap();
        let mut measurement: Value = serde_json::from_slice(&raw).unwrap();
        completion["raw_path"] = json!("measurement.raw.json");
        completion["raw_sha256"] = json!(format!("{:x}", Sha256::digest(&raw)));
        completion["raw_json"] = json!(String::from_utf8(raw).unwrap());
        measurement["observations"]["cleanup"] = completion;
        let mut final_file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(required("RM4F_MEASUREMENT"))
            .unwrap();
        serde_json::to_writer_pretty(&mut final_file, &measurement).unwrap();
        final_file.flush().unwrap();
        final_file.sync_all().unwrap();
    }
}
