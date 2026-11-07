// SPDX-License-Identifier: MIT
#![cfg(test)]
//! One reservation on one continuously admitted connection; retry is independent evidence.
use super::*;
use crate::commands::host_connection_manager::rm4f_support::OwnedManagerHarness;
use crate::terminal::session_server::Rm4fSnapshot;
use codesense_pty_core::domain::TerminalDomain;
use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};
use codesense_session_server::testing::{registration, sh, sh_spec, RecordingSink, TestServer};
use serde_json::json;
use std::time::{Duration, Instant};

const REJECTION: &str = "rm4f final authority rejection";
const SESSION: &str = "rm4f-interrupted-reservation";
const LOCAL_SESSIONS: [&str; 2] = ["rm4f-local-before", "rm4f-local-after"];

fn local_control_spec(session: &str, nonce: &str) -> codesense_pty_core::runner::PtySpec {
    // No subprocess can escape the nonce: argv is installed before spawn and
    // the blocking control uses only shell builtins for its entire lifetime.
    let mut spec = sh_spec(session, "");
    spec.command = portable_pty::CommandBuilder::new("/bin/sh");
    spec.command.args([
        "-c",
        "while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
        nonce,
    ]);
    spec.command.cwd("/tmp");
    spec
}

struct LocalControlGuard {
    host: Arc<TerminalHost>,
    armed: std::cell::Cell<bool>,
}

impl LocalControlGuard {
    fn new(host: Arc<TerminalHost>) -> Self {
        Self {
            host,
            armed: std::cell::Cell::new(true),
        }
    }

    fn finish(&self) -> bool {
        for name in LOCAL_SESSIONS {
            let _ = self.host.kill(&PtySessionId::new(name));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if LOCAL_SESSIONS
                .iter()
                .all(|name| self.host.has_session(&PtySessionId::new(*name)) == Ok(false))
            {
                self.armed.set(false);
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for LocalControlGuard {
    fn drop(&mut self) {
        if self.armed.get() && !self.finish() {
            eprintln!("RM4F local control cleanup was not observed complete");
        }
    }
}

#[derive(Default)]
struct LocalBranches {
    spawn: std::cell::Cell<u64>,
    warm_branch: std::cell::Cell<u64>,
    fallback: std::cell::Cell<u64>,
}

impl LocalBranches {
    fn snapshot(&self) -> serde_json::Value {
        json!({"spawn": self.spawn.get(), "warm_branch": self.warm_branch.get(),
               "fallback": self.fallback.get()})
    }

    async fn warm(&self) -> Result<bool, String> {
        // Observe the production helper's sole local warm-work branch, not
        // synthetic acquire/refill operations. An uncalled branch proves both
        // operations unreachable inside the remote failure window.
        self.warm_branch.set(self.warm_branch.get() + 1);
        Ok(false)
    }
}

fn native_positive_probe(pid: u32, nonce: &str) -> Result<serde_json::Value, String> {
    let mut child =
        std::process::Command::new(std::env::var("RM4F_NATIVE_PYTHON").map_err(|e| e.to_string())?)
            .args([
                std::env::var("RM4F_NATIVE_HELPER").map_err(|e| e.to_string())?,
                "probe".into(),
                std::env::var("RM4F_RUN_ID").map_err(|e| e.to_string())?,
                pid.to_string(),
            ])
            .env("RM4F_NATIVE_CONTROL_NONCE", nonce)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("native positive probe failed or timed out".into());
            }
        }
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "native positive probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

fn local_control(
    runtime: &tokio::runtime::Runtime,
    local: &Arc<TerminalHost>,
    unavailable_sidecar: &ResolvedTerminalBackend,
    branches: &LocalBranches,
    session: &str,
) -> serde_json::Value {
    let control = std::cell::RefCell::new(None);
    open_tab_resolved(
        unavailable_sidecar.clone(),
        request(session),
        |placement| {
            runtime
                .block_on(crate::commands::pty::attach_warm_from_args_core(
                    &SpawnPlacement::Exact(Box::new(placement)),
                    || branches.warm(),
                    || async {
                        branches.spawn.set(branches.spawn.get() + 1);
                        let spec = local_control_spec(
                            session,
                            &std::env::var("RM4F_NATIVE_CONTROL_NONCE")
                                .expect("parent-owned local control nonce"),
                        );
                        let nonce = spec
                            .command
                            .get_argv()
                            .last()
                            .and_then(|arg| arg.to_str())
                            .expect("actual nonce argv")
                            .to_owned();
                        let pty = LocalPtyRunner
                            .spawn(spec)
                            .await
                            .map_err(|e| e.to_string())?;
                        let pid = pty.pid().ok_or("local control has no PID")?;
                        local
                            .register(pty, registration(session, None, false))
                            .map_err(|e| e.to_string())?;
                        let probe = native_positive_probe(pid, &nonce)?;
                        *control.borrow_mut() = Some(
                            json!({"session": session, "pid": pid, "nonce": nonce, "probe": probe}),
                        );
                        Ok(())
                    },
                ))
                .map(|_| ())
        },
        |_| Ok(()),
        || {
            branches.fallback.set(branches.fallback.get() + 1);
            Ok((local.clone(), local_in_process_backend(local.clone())?))
        },
    )
    .unwrap();
    assert!(local.has_session(&PtySessionId::new(session)).unwrap());
    control.into_inner().expect("local control positive proof")
}

fn remote_intent() -> SpawnIntent {
    let mut intent = sh("sleep 60; :");
    // Debian has the fixture account's home, not the local helper's /var/empty.
    intent.cwd = "/home/dev".into();
    intent.env.clear();
    intent
}

fn remove_remote(client: &SessionServerClient, session: &PtySessionId) -> bool {
    if client.kill(session).is_err() {
        return false;
    }
    // A 5s polling deadline plus at most one 3s bounded HasSession call.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match client.has_session(session) {
            Ok(false) => return true,
            Ok(true) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => return false,
        }
    }
}

fn remote_control(client: &SessionServerClient, name: &str) {
    let session = PtySessionId::new(name);
    client
        .spawn(remote_intent(), registration(name, None, false))
        .unwrap();
    client.write(&session, b"rm4f-observer-control\n").unwrap();
    assert!(
        remove_remote(client, &session),
        "remote positive control cleanup"
    );
    client.heartbeat(Duration::from_secs(5)).unwrap();
}

fn quiescent(client: &SessionServerClient) -> Rm4fSnapshot {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let before = settled(client);
        std::thread::sleep(Duration::from_millis(100));
        let after = settled(client);
        if serde_json::to_value(&before).unwrap() == serde_json::to_value(&after).unwrap() {
            return after;
        }
        assert!(
            Instant::now() < deadline,
            "fixture did not become quiescent"
        );
    }
}

fn write_measurement(path: &str, document: &serde_json::Value) {
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .unwrap();
    serde_json::to_writer_pretty(&mut file, document).unwrap();
    std::io::Write::flush(&mut file).unwrap();
}

fn request(session: &str) -> OpenTab {
    OpenTab {
        viewer: crate::commands::pty_layout::app_viewer(),
        workspace: codesense_pty_core::domain::WorkspaceKey::new("root:rm4f/reservation"),
        session: PtySessionId::new(session),
        descriptor: Some(codesense_pty_core::layout::WorkspaceDescriptor {
            display_name: "reservation oracle".into(),
            kind: "root".into(),
            ..Default::default()
        }),
        name: None,
        role: None,
        tags: Default::default(),
        placement: codesense_pty_core::layout::Placement::Tab,
        index: None,
        activate: true,
    }
}

// A reply can race the writer's post-flush observation. Await only already-read
// replies' frame records, without introducing another request into the window.
fn settled(client: &SessionServerClient) -> Rm4fSnapshot {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = client.rm4f_snapshot();
        if snapshot
            .replies
            .iter()
            .all(|reply| snapshot.requests.iter().any(|r| r.rid == reply.rid))
        {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "writer observation did not settle"
        );
        std::thread::yield_now();
    }
}

fn count(snapshot: &Rm4fSnapshot, baseline: &Rm4fSnapshot, kind: &str) -> usize {
    snapshot.requests[baseline.requests.len()..]
        .iter()
        .filter(|r| r.kind == kind)
        .count()
}

fn acknowledged(
    snapshot: &Rm4fSnapshot,
    baseline: &Rm4fSnapshot,
    kind: &str,
    success: bool,
) -> usize {
    snapshot.requests[baseline.requests.len()..]
        .iter()
        .filter(|request| {
            request.kind == kind
                && request.session.as_deref() == Some(SESSION)
                && snapshot.replies[baseline.replies.len()..]
                    .iter()
                    .any(|reply| {
                        reply.rid == request.rid
                            && reply.success == success
                            && (success || reply.error.as_deref() == Some(REJECTION))
                            && reply.transport_success
                            && match &reply.reply {
                                Some(codesense_pty_core::protocol::Reply::Done) => success,
                                Some(codesense_pty_core::protocol::Reply::LayoutRefused {
                                    ..
                                }) => !success,
                                _ => false,
                            }
                    })
        })
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires C2-owned A/B fresh slots"]
async fn rm4f_03_reservation_abort() {
    let harness = OwnedManagerHarness::supplied().await.unwrap();
    let mut document = harness.with_cleanup(async {
        harness.connect(0).await.unwrap();
        harness.connect(1).await.unwrap();
        let a = resolve_remote_backend(&harness.manager, &harness.machines.ids[0]).unwrap();
        let b = resolve_remote_backend(&harness.manager, &harness.machines.ids[1]).unwrap();
        let manager = harness.manager.clone();
        crate::commands::await_blocking("measure interrupted remote reservation", move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let local = tests::in_process(&runtime);
            let local_controls = LocalControlGuard::new(local.clone());
            let local_identity = local.identity().unwrap();
            let local_server = TestServer::start();
            let local_client = SessionServerState::default().connect_or_launch(
                local_server.root(), Arc::new(RecordingSink::default()), |_| Err("no launch".into())).unwrap();
            let unavailable_local = select_local_backend(Some(local_client.clone()), || panic!("sidecar selected")).unwrap();
            // Positive fallback control: this local sidecar has no reservations.
            // A and B stay on their original live connections throughout.
            local_client.sever();
            let branches = LocalBranches::default();
            let local_before_control = branches.snapshot();
            let local_probe_before = local_control(&runtime, &local, &unavailable_local, &branches, "rm4f-local-before");
            let client = a.client().unwrap();
            let other = b.client().unwrap();
            client.rm4f_observe();
            other.rm4f_observe();
            let before_control = [settled(&client), settled(&other)];
            remote_control(&client, "rm4f-a-before");
            remote_control(&other, "rm4f-b-before");
            client.heartbeat(Duration::from_secs(5)).unwrap();
            other.heartbeat(Duration::from_secs(5)).unwrap();
            let sessions_a = client.live_session_ids().unwrap();
            let sessions_b = other.live_session_ids().unwrap();
            assert!(sessions_a.is_empty() && sessions_b.is_empty(), "quiescent fresh hosts required");
            let local_sessions = local.live_session_ids().unwrap();
            assert_eq!(local_sessions.len(), 1);
            let local_before = branches.snapshot();
            let before = [quiescent(&client), quiescent(&other)];
            let mutant = std::env::var("RM4F_MUTANT").ok();
            assert!(mutant.as_deref().is_none_or(|name| name == "rm4f_skip_interrupted_reservation_abort"));
            client.rm4f_skip_abort(mutant.is_some());
            let session = SESSION;
            let final_send = std::cell::Cell::new(0u64);
            let validate = |backend: &ResolvedTerminalBackend| manager.revalidate_backend(backend.remote_token.as_ref().unwrap())
                .map_err(|error| map_connected_error(&backend.machine_id, error).to_string());
            let result = open_tab_resolved(a.clone(), request(session), |placement| {
                runtime.block_on(crate::commands::pty::attach_warm_from_args_core(
                    &SpawnPlacement::Exact(Box::new(placement.clone())), || branches.warm(), || async {
                        send_on_exact_backend(&placement, |backend| {
                            validate(backend)?;
                            Err(REJECTION.into())
                        }, |_| { final_send.set(final_send.get() + 1); Ok(()) })
                        .map_err(|failure| failure.message)
                    })).map(|_| ())
            }, validate, || {
                branches.fallback.set(branches.fallback.get() + 1);
                Ok((local.clone(), local_in_process_backend(local.clone())?))
            });
            let failed = [settled(&client), settled(&other)];
            let local_failed = branches.snapshot();
            let local_sessions_failed = local.live_session_ids().unwrap();
            assert_eq!(local_sessions_failed, local_sessions);
            client.rm4f_skip_abort(false);
            let retry = open_tab_resolved(a.clone(), request(session), |placement| {
                send_on_exact_backend(&placement, validate, |selected| {
                    selected.spawn(remote_intent(), registration(session, None, false)).map(|_| ())
                }).map_err(|failure| failure.message)
            }, validate, || panic!("remote retry cannot fall back"));
            let retried = settled(&client);
            let retry_accepted = acknowledged(&retried, &failed[0], "reserve_tab", true) == 1;
            let cleanup = retry.is_ok() && remove_remote(&client, &PtySessionId::new(session));
            remote_control(&client, "rm4f-a-after");
            remote_control(&other, "rm4f-b-after");
            client.heartbeat(Duration::from_secs(5)).unwrap();
            other.heartbeat(Duration::from_secs(5)).unwrap();
            let local_probe_after = local_control(&runtime, &local, &unavailable_local, &branches, "rm4f-local-after");
            let local_after_control = branches.snapshot();
            let local_sessions_after = local.live_session_ids().unwrap();
            assert!(local.has_session(&PtySessionId::new("rm4f-local-before")).unwrap());
            assert!(local_controls.finish(), "local control cleanup did not complete");
            let after_control = [settled(&client), settled(&other)];
            let checks = json!({
                "rejected": result.as_ref().err().map(String::as_str) == Some(REJECTION),
                "reserve_acknowledged": count(&failed[0], &before[0], "reserve_tab") == 1 && acknowledged(&failed[0], &before[0], "reserve_tab", true) == 1,
                "cleanup_acknowledged": count(&failed[0], &before[0], "finish_tab_error") == 1 && acknowledged(&failed[0], &before[0], "finish_tab_error", false) == 1,
                "no_spawn": count(&failed[0], &before[0], "spawn") == 0 && final_send.get() == 0,
                "no_input": failed[0].input_payload_bytes == before[0].input_payload_bytes,
                "b_quiet": serde_json::to_value(&failed[1]).unwrap() == serde_json::to_value(&before[1]).unwrap(),
                "no_fallback": local_failed["fallback"] == local_before["fallback"],
                "local_quiet": local_failed == local_before,
                "retry_accepted": retry_accepted,
                "retry_completed": retry.is_ok() && cleanup,
                "same_connection": client.is_alive() && manager.revalidate_backend(a.remote_token.as_ref().unwrap()).is_ok(),
            });
            let document = json!({
                "version": 1, "run_id": std::env::var("RM4F_RUN_ID").unwrap(),
                "scenario_id": "RM4F-03", "mutant": mutant,
                "observations": {
                    "a": {"machine_id": a.machine_id, "server_id": client.server_id(), "epoch": client.epoch(), "generation": client.namespace().generation(),
                          "sessions": sessions_a.len(), "before_control": before_control[0], "before": before[0], "failed": failed[0], "retried": retried, "after_control": after_control[0]},
                    "b": {"machine_id": b.machine_id, "server_id": other.server_id(), "epoch": other.epoch(), "generation": other.namespace().generation(),
                          "sessions": sessions_b.len(), "before_control": before_control[1], "before": before[1], "failed": failed[1], "after_control": after_control[1]},
                    "local": {"machine_id": LOCAL_MACHINE_ID, "server_id": local_identity.server.id,
                              "epoch": local_identity.epoch, "sessions": local_sessions.len(),
                              "session_ids_before": local_sessions, "session_ids_failed": local_sessions_failed,
                              "session_ids_after_control": local_sessions_after,
                              "controls": [local_probe_before, local_probe_after],
                              "before_control": local_before_control, "before": local_before,
                              "failed": local_failed, "after_control": local_after_control},
                    "session": session, "final_send": final_send.get(),
                    "rejection": result.err(), "retry_completed": retry.is_ok() && cleanup,
                    "connection_after": {"server_id": client.server_id(), "epoch": client.epoch(),
                        "generation": client.namespace().generation(), "alive": client.is_alive(),
                        "revalidated": manager.revalidate_backend(a.remote_token.as_ref().unwrap()).is_ok()}
                }, "outcome_checks": checks
            });
            let path = std::env::var("RM4F_MEASUREMENT_PATH").unwrap();
            write_measurement(&format!("{path}.before-cleanup"), &document);
            Ok(document)
        }).await.unwrap()
    }).await;
    // A measured product assertion is deliberately after the awaited owner
    // drain. A missing marker can never masquerade as mutant detection.
    let joined = json!({
        "version": 1, "run_id": std::env::var("RM4F_RUN_ID").unwrap(),
        "scenario_id": "RM4F-03", "joined": true
    });
    write_measurement(&std::env::var("RM4F_CLEANUP_PATH").unwrap(), &joined);
    document["observations"]["ownership_cleanup"] = joined;
    write_measurement(&std::env::var("RM4F_MEASUREMENT_PATH").unwrap(), &document);
    let passed = document["outcome_checks"]
        .as_object()
        .unwrap()
        .values()
        .all(|value| value == true);
    assert!(passed, "reservation measurements falsify the contract");
}

#[test]
fn local_controls_join_on_unwind() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let local = tests::in_process(&runtime);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _controls = LocalControlGuard::new(local.clone());
        let pty = runtime
            .block_on(LocalPtyRunner.spawn(local_control_spec(
                LOCAL_SESSIONS[0],
                &format!("rm4f-unwind-{}", uuid::Uuid::new_v4()),
            )))
            .unwrap();
        local
            .register(pty, registration(LOCAL_SESSIONS[0], None, false))
            .unwrap();
        assert!(local
            .has_session(&PtySessionId::new(LOCAL_SESSIONS[0]))
            .unwrap());
        panic!("exercise local control unwind");
    }));
    assert!(result.is_err());
    assert_eq!(
        local.has_session(&PtySessionId::new(LOCAL_SESSIONS[0])),
        Ok(false)
    );
}
