// SPDX-License-Identifier: MIT
//! SW-XS-6: the desktop's `DestinationIo::manifest()` / `apply_batch()` over
//! the production client, framing and request-id dispatch, against a real
//! prepared workspace answered by the production session-server handlers.
//! Calls are counted at the client's request-id dispatch (SPEC §11.14), never
//! at the `DestinationIo` trait.
use super::*;
use crate::content_engine::destination::*;
use codesense_pty_core::context_batch::{MemberOutcome, OperationKind, TransferPhase};
use codesense_pty_core::remote_files::EffectOutcome;
use codesense_session_server::testing::RemoteWorkspaceFixture;
use std::os::unix::fs::MetadataExt;

struct Wire {
    /// SW-XS-26: each Begin's codec and wire length, as the host received it.
    begins: Arc<Mutex<Vec<(codesense_pty_core::context_batch::Codec, u64, u64)>>>,
    /// SW-XS-26: answer the Seal of a zstd batch as an old or strict host
    /// that refuses the frame would.
    refuse_zstd_seal: Arc<AtomicBool>,
    fixture: Arc<RemoteWorkspaceFixture>,
    client: Arc<SessionServerClient>,
    io: Arc<dyn DestinationIo>,
    journal: tempfile::TempDir,
    host: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Wire {
    fn drop(&mut self) {
        self.client.writer.close();
        if let Some(host) = self.host.take() {
            let _ = host.join();
        }
    }
}
impl Wire {
    fn new(capabilities: Vec<Capability>) -> Self {
        let fixture = Arc::new(RemoteWorkspaceFixture::new());
        let root = short_root();
        let ((client_reader, client_writer), (mut host_reader, mut host_writer)) =
            memory_transport_pair();
        let serving = fixture.clone();
        let begins = Arc::new(Mutex::new(Vec::new()));
        let seen = begins.clone();
        let refuse_zstd_seal = Arc::new(AtomicBool::new(false));
        let refusing = refuse_zstd_seal.clone();
        let mut zstd_batches = std::collections::HashSet::new();
        let host = std::thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut host_reader) {
                let Ok(Message::Call { rid, request }) = frame.decode_json::<Message>() else {
                    continue;
                };
                if let Request::WorkspaceContextBatch { request: batch } = &request {
                    use codesense_pty_core::context_batch::{BatchOperation, Codec};
                    match &batch.operation {
                        BatchOperation::Begin { manifest } => {
                            seen.lock().unwrap().push((
                                manifest.codec,
                                manifest.raw_length,
                                manifest.encoded_length,
                            ));
                            if manifest.codec == Codec::Zstd {
                                zstd_batches.insert(batch.batch_id.clone());
                            }
                        }
                        BatchOperation::Seal
                            if refusing.load(Ordering::SeqCst)
                                && zstd_batches.contains(&batch.batch_id) =>
                        {
                            let _ =
                                write_frame(
                                    &mut host_writer,
                                    &Frame::control(&Message::Reply {
                                        rid,
                                        result: Err(
                                            "zstd frame window exceeds the host's limit".into()
                                        ),
                                    })
                                    .unwrap(),
                                );
                            continue;
                        }
                        _ => {}
                    }
                }
                let mut send = |frame: Frame| {
                    let _ = write_frame(&mut host_writer, &frame);
                };
                if matches!(request, Request::Listen) {
                    send(
                        Frame::control(&Message::Reply {
                            rid,
                            result: Ok(Reply::Done),
                        })
                        .unwrap(),
                    );
                    continue;
                }
                if serving.answer(rid, request.clone(), &mut send).is_none() {
                    panic!("unexpected request {request:?}");
                }
            }
        });
        let interrupted = client_writer.0.clone();
        let mut transport = SessionServerTransport::new(client_reader, client_writer, move || {
            interrupted.closed.store(true, Ordering::SeqCst);
            interrupted.ready.notify_all();
        });
        let (lease, _revoke) = crate::commands::auth::private_authority::test_lease(41);
        transport.private_lease = Some(lease);
        let mut hello = mock_hello(instance_id(root.path()));
        hello.capabilities = capabilities;
        let client = SessionServerClient::from_admitted_transport(
            root.path(),
            AdmittedSessionTransport {
                transport,
                hello,
                remote_close: None,
            },
            Arc::new(RecordingSink::default()),
            Arc::default(),
            None,
        )
        .unwrap();
        client.rm4f_observe();
        let journal = tempfile::tempdir().unwrap();
        let io = adapter(&fixture, &client, journal.path());
        Self {
            begins,
            refuse_zstd_seal,
            fixture,
            client,
            io,
            journal,
            host: Some(host),
        }
    }
    /// The same destination as a fresh process sees it: nothing in memory,
    /// only the journal on disk.
    fn restarted(&self) -> Arc<dyn DestinationIo> {
        adapter(&self.fixture, &self.client, self.journal.path())
    }
    fn current() -> Self {
        Self::new(Self::current_capabilities())
    }
    fn current_capabilities() -> Vec<Capability> {
        vec![
            Capability::Terminals,
            Capability::SelectedContextBatchV1,
            Capability::ContextManifestV1,
            Capability::ManifestWorkspaceExcludeV1,
            Capability::ManifestReadV1,
        ]
    }
    /// The workspace calls sent so far, by kind, as dispatched by request id.
    fn calls(&self) -> Vec<String> {
        self.client
            .rm4f_snapshot()
            .requests
            .into_iter()
            .map(|request| request.kind)
            .filter(|kind| kind.starts_with("workspace_") || kind.starts_with("batch_"))
            .collect()
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.fixture.worktree.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}
fn adapter(
    fixture: &RemoteWorkspaceFixture,
    client: &Arc<SessionServerClient>,
    journal: &std::path::Path,
) -> Arc<dyn DestinationIo> {
    crate::mcp::remote_destination::from_admitted_with_journal_for_test(
        RemoteDestinationIdentity {
            owner_id: 41,
            machine_id: "devbox".into(),
            server_id: "server".into(),
            workspace_handle: fixture.target.handle.clone(),
        },
        fixture.target.clone(),
        client.clone(),
        Arc::new(|| Ok(())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(Mutex::new(None)),
        Some(journal.to_path_buf()),
    )
}
fn prefixes(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}
fn relative(path: &str) -> DestinationRelativePath {
    DestinationRelativePath::try_from(path.to_string()).unwrap()
}
fn hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// AC1 + AC5: the desktop's `manifest()` for 2,000 files under 3 prefixes
/// sends exactly ONE `WorkspaceManifest` frame; the first hashes 2,000, the
/// second hashes 0.
#[test]
fn desktop_manifest_of_two_thousand_files_is_one_frame_and_then_all_cache_hits() {
    let wire = Wire::current();
    for index in 0..2_000 {
        let prefix = [".claude/skills", "docs", "_bmad"][index % 3];
        wire.write(
            &format!("{prefix}/d{}/f{index}.md", index % 13),
            format!("context {index}").as_bytes(),
        );
    }
    // Written before the walk, as a workspace's files are.
    std::thread::sleep(Duration::from_millis(120));
    let wanted = prefixes(&[".claude/skills", "docs", "_bmad"]);
    let first = wire.io.manifest(&wanted, 10_000, &mut |_| {}).unwrap();
    assert_eq!(
        wire.calls(),
        vec!["workspace_manifest"],
        "exactly one frame"
    );
    assert_eq!(first.files.len(), 2_000);
    assert!(first.skipped.is_empty());
    assert_eq!(first.prefixes, vec![".claude/skills", "_bmad", "docs"]);
    assert_eq!(first.progress.hashed, 2_000);
    let entry = first
        .files
        .iter()
        .find(|entry| entry.path == "docs/d1/f1.md")
        .unwrap();
    assert_eq!(entry.hash, Some(hash(b"context 1")));
    let second = wire.io.manifest(&wanted, 10_000, &mut |_| {}).unwrap();
    assert_eq!(wire.calls().len(), 2);
    assert_eq!(second.progress.hashed, 0, "cache-hit counter");
    assert_eq!(second.progress.cache_hits, 2_000);
    assert_eq!(second.files, first.files);
}

/// A cold 6,000-file manifest streams progress to the caller while the host
/// hashes, and completes in one call.
#[test]
fn a_cold_six_thousand_file_manifest_streams_progress_over_the_wire() {
    let wire = Wire::current();
    for index in 0..6_000 {
        wire.write(&format!("docs/d{}/f{index}.md", index % 40), &[b'y'; 128]);
    }
    let mut updates = Vec::new();
    let manifest = wire
        .io
        .manifest(&prefixes(&["docs"]), 10_000, &mut |progress| {
            updates.push(*progress)
        })
        .unwrap();
    assert_eq!(manifest.files.len(), 6_000);
    assert!(updates.len() >= 5, "progress streamed: {}", updates.len());
    assert!(updates
        .windows(2)
        .all(|pair| pair[0].scanned <= pair[1].scanned));
    assert_eq!(wire.calls(), vec!["workspace_manifest"]);
}

/// AC4: a server without `ContextManifestV1` gets the update message and
/// sees no call at all — no per-file fallback. A batch that needs nothing new
/// still goes to it (the gate applies only when the diff needs the new ops).
#[test]
fn an_old_server_gets_the_update_message_and_no_per_file_calls() {
    let wire = Wire::new(vec![
        Capability::Terminals,
        Capability::SelectedContextBatchV1,
    ]);
    for index in 0..50 {
        wire.write(&format!("docs/{index}.md"), b"old server");
    }
    let refused = wire
        .io
        .manifest(&prefixes(&["docs"]), 1_000, &mut |_| {})
        .unwrap_err();
    assert_eq!(
        refused,
        DestinationBatchError::ServerOutdated {
            machine_id: "devbox".into()
        }
    );
    assert!(refused
        .to_string()
        .contains("Update the terminal server on devbox"));
    assert!(DestinationBatchError::names_outdated_server(
        &refused.to_string()
    ));
    let needs_new_ops = DestinationBatch {
        batch_id: uuid::Uuid::new_v4(),
        writes: vec![],
        removes: vec![BatchRemove {
            operation_id: uuid::Uuid::new_v4(),
            path: relative("docs/1.md"),
            expected_pre_hash: hash(b"old server"),
        }],
        chmods: vec![],
        ensure_dirs: vec![],
        exclude: None,
    };
    assert!(matches!(
        wire.io.apply_batch(&needs_new_ops, &mut |_, _| true),
        Err(DestinationBatchError::ServerOutdated { .. })
    ));
    assert!(wire.calls().is_empty(), "zero calls: {:?}", wire.calls());
    assert!(wire.fixture.worktree.join("docs/1.md").exists());
    // Writes only, inside the original caps: no gate.
    let writes_only = DestinationBatch {
        batch_id: uuid::Uuid::new_v4(),
        writes: (0..2)
            .map(|index| BatchWrite {
                operation_id: uuid::Uuid::new_v4(),
                path: relative(&format!("new/{index}.md")),
                bytes: b"fits".to_vec(),
                mode: 0o644,
                expected: ExpectedDestination::Absent,
            })
            .collect(),
        removes: vec![],
        chmods: vec![],
        ensure_dirs: vec![],
        exclude: None,
    };
    let outcome = wire.io.apply_batch(&writes_only, &mut |_, _| true).unwrap();
    assert!(outcome.writes.iter().all(|write| matches!(
        write.outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Applied { .. }
        }
    )));
}

fn applied(outcome: &MemberOutcome) -> bool {
    matches!(
        outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Applied { .. }
        }
    )
}

/// AC2 over the wire: writes, removes, chmods, a directory and the exclude
/// block in ONE Apply; a stale pre-hash fails only its own entry. Then the
/// same attempt again replays its stored outcomes (idempotent by operation
/// id) without dispatching anything new.
#[test]
fn one_apply_over_the_wire_and_an_idempotent_replay() {
    let wire = Wire::current();
    wire.write("ctx/old.md", b"old");
    wire.write("ctx/run.sh", b"#!/bin/sh\n");
    wire.write("ctx/w3.md", b"edited by the user");
    let writes: Vec<BatchWrite> = (0..10)
        .map(|index| BatchWrite {
            operation_id: uuid::Uuid::new_v4(),
            path: relative(&format!("ctx/w{index}.md")),
            bytes: format!("write {index}").into_bytes(),
            mode: 0o644,
            expected: if index == 3 {
                ExpectedDestination::Hash(hash(b"what the diff saw"))
            } else {
                ExpectedDestination::Absent
            },
        })
        .collect();
    let batch = DestinationBatch {
        batch_id: uuid::Uuid::new_v4(),
        writes,
        removes: vec![BatchRemove {
            operation_id: uuid::Uuid::new_v4(),
            path: relative("ctx/old.md"),
            expected_pre_hash: hash(b"old"),
        }],
        chmods: vec![BatchChmod {
            operation_id: uuid::Uuid::new_v4(),
            path: relative("ctx/run.sh"),
            mode: 0o755,
            expected_pre_hash: hash(b"#!/bin/sh\n"),
        }],
        ensure_dirs: vec![BatchDir {
            operation_id: uuid::Uuid::new_v4(),
            path: relative("ctx/empty"),
        }],
        exclude: Some(BatchExclude {
            operation_id: uuid::Uuid::new_v4(),
            workspace_id: "ws-wire".into(),
            expected: vec![],
            paths: (0..10).map(|index| format!("ctx/w{index}.md")).collect(),
            repair: false,
        }),
    };
    let outcome = wire.io.apply_batch(&batch, &mut |_, _| true).unwrap();
    let calls = wire.calls();
    assert_eq!(
        calls.iter().filter(|kind| *kind == "batch_apply").count(),
        1,
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|kind| kind == "workspace_file"),
        "{calls:?}"
    );
    assert_eq!(outcome.phase, TransferPhase::Finished);
    let failed: Vec<_> = outcome
        .writes
        .iter()
        .filter(|write| !applied(&write.outcome))
        .map(|write| write.index)
        .collect();
    assert_eq!(failed, vec![3]);
    assert!(outcome.operations.iter().all(|op| applied(&op.outcome)));
    assert!(outcome
        .operations
        .iter()
        .any(|op| op.kind == OperationKind::ExcludeBlock));
    let worktree = &wire.fixture.worktree;
    assert!(!worktree.join("ctx/old.md").exists());
    assert_eq!(
        std::fs::metadata(worktree.join("ctx/run.sh"))
            .unwrap()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::read(worktree.join("ctx/w3.md")).unwrap(),
        b"edited by the user"
    );
    let exclude = std::fs::read_to_string(wire.fixture.repo.join(".git/info/exclude")).unwrap();
    assert!(exclude.contains("# >>> codesense managed workspace ws-wire"));
    assert!(exclude.contains("/ctx/w0.md") && !exclude.contains("/ctx/w3.md"));
    // The user recreates the removed file; a replay of the same attempt
    // returns the stored outcomes and touches nothing.
    wire.write("ctx/old.md", b"old");
    let before = wire.calls().len();
    let replay = wire.io.apply_batch(&batch, &mut |_, _| true).unwrap();
    assert_eq!(replay.writes, outcome.writes);
    assert_eq!(replay.operations, outcome.operations);
    assert_eq!(wire.calls()[before..], ["batch_begin".to_string()]);
    assert!(worktree.join("ctx/old.md").exists());
}

/// AC3 over the wire: a 50 MB batch streams in chunks with no item or byte
/// cap error.
#[test]
fn a_fifty_megabyte_batch_streams_over_the_wire_without_a_cap_error() {
    let wire = Wire::current();
    let member: usize = 2 * 1024 * 1024;
    let mut seed = 7u32;
    let writes: Vec<BatchWrite> = (0..25)
        .map(|index| BatchWrite {
            operation_id: uuid::Uuid::new_v4(),
            path: relative(&format!("big/{index}.bin")),
            bytes: (0..member)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect(),
            mode: 0o644,
            expected: ExpectedDestination::Absent,
        })
        .collect();
    let last = writes[24].bytes.clone();
    let batch = DestinationBatch {
        batch_id: uuid::Uuid::new_v4(),
        writes,
        removes: vec![],
        chmods: vec![],
        ensure_dirs: vec![],
        exclude: None,
    };
    let mut sent = 0;
    let outcome = wire
        .io
        .apply_batch(&batch, &mut |offset, total| {
            assert_eq!(total, 25 * member as u64);
            sent = offset;
            true
        })
        .unwrap();
    assert_eq!(sent, 25 * member as u64);
    assert!(outcome.writes.iter().all(|write| applied(&write.outcome)));
    assert_eq!(
        std::fs::read(wire.fixture.worktree.join("big/24.bin")).unwrap(),
        last
    );
    let calls = wire.calls();
    assert_eq!(
        calls.iter().filter(|kind| *kind == "batch_apply").count(),
        1
    );
    assert_eq!(
        calls.iter().filter(|kind| *kind == "batch_chunk").count(),
        (25 * member).div_ceil(codesense_pty_core::context_batch::CHUNK_BYTES)
    );
}

fn small_writes(count: usize, prefix: &str) -> Vec<BatchWrite> {
    (0..count)
        .map(|index| BatchWrite {
            operation_id: uuid::Uuid::new_v4(),
            path: relative(&format!("{prefix}/{index}.md")),
            bytes: format!("{prefix} {index}").into_bytes(),
            mode: 0o644,
            expected: ExpectedDestination::Absent,
        })
        .collect()
}
fn writes_only(writes: Vec<BatchWrite>) -> DestinationBatch {
    DestinationBatch {
        batch_id: uuid::Uuid::new_v4(),
        writes,
        removes: vec![],
        chmods: vec![],
        ensure_dirs: vec![],
        exclude: None,
    }
}

/// M8: an incomplete manifest is not a manifest: it is an error the diff
/// cannot use, so removals are never computed from it.
#[test]
fn an_incomplete_manifest_is_an_error_not_a_manifest() {
    let wire = Wire::current();
    wire.write("docs/open.md", b"open");
    wire.write("docs/locked/inner.md", b"locked");
    let locked = wire.fixture.worktree.join("docs/locked");
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    let result = wire.io.manifest(&prefixes(&["docs"]), 100, &mut |_| {});
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    assert!(
        matches!(result, Err(DestinationBatchError::Incomplete { ref reason }) if reason.contains("docs/locked")),
        "{result:?}"
    );
}

/// H3: a write larger than one member may be is its own `TooLarge` outcome;
/// the rest of the batch applies in the one Apply.
#[test]
fn an_oversize_write_is_reported_per_entry_and_the_rest_applies() {
    let wire = Wire::current();
    let mut writes = small_writes(2, "fit");
    writes.insert(
        1,
        BatchWrite {
            operation_id: uuid::Uuid::new_v4(),
            path: relative("big/huge.bin"),
            bytes: vec![1; codesense_pty_core::remote_files::MAX_FILE_BYTES + 1],
            mode: 0o644,
            expected: ExpectedDestination::Absent,
        },
    );
    let outcome = wire
        .io
        .apply_batch(&writes_only(writes), &mut |_, _| true)
        .unwrap();
    assert_eq!(outcome.writes.len(), 3);
    assert!(applied(&outcome.writes[0].outcome) && applied(&outcome.writes[2].outcome));
    assert!(matches!(
        outcome.writes[1].outcome,
        MemberOutcome::TooLarge { size, .. } if size as usize == codesense_pty_core::remote_files::MAX_FILE_BYTES + 1
    ));
    assert!(!wire.fixture.worktree.join("big/huge.bin").exists());
    assert!(wire.fixture.worktree.join("fit/1.md").exists());
}

/// H2: an Apply that outlives one control call's deadline is not a failure:
/// asking again waits behind it, then reports its stored outcomes.
#[test]
fn an_apply_slower_than_one_call_deadline_still_settles() {
    let wire = Wire::current();
    let release = wire.fixture.hold_next_apply();
    let io = wire.io.clone();
    let batch = writes_only(small_writes(5, "slow"));
    let caller = std::thread::spawn(move || {
        APPLY_WAIT_OVERRIDE
            .with(|wait| wait.set(Some((Duration::from_millis(200), Duration::from_secs(60)))));
        io.apply_batch(&batch, &mut |_, _| true)
    });
    // The machine holds the first Apply past its call's deadline; the desktop
    // has already asked again by the time it finishes.
    assert!(codesense_session_server::testing::wait_until(|| {
        wire.calls()
            .iter()
            .filter(|kind| *kind == "batch_apply")
            .count()
            >= 2
    }));
    release.send(()).unwrap();
    let outcome = caller.join().unwrap().unwrap();
    assert!(outcome.writes.iter().all(|write| applied(&write.outcome)));
    assert_eq!(outcome.phase, TransferPhase::Finished);
    // Nothing is left for the next call to recover.
    assert_eq!(std::fs::read_dir(wire.journal.path()).unwrap().count(), 0);
}

/// H2: the desktop dies while its batch is applying. The next process's next
/// batch to that destination first resumes and settles the cut one (from the
/// journal), then runs its own.
#[test]
fn a_batch_cut_mid_apply_is_recovered_by_the_next_process() {
    let wire = Wire::current();
    let cut = writes_only(small_writes(3, "cut"));
    wire.fixture
        .cut_apply_when(Some(wire.fixture.worktree.join("cut/0.md")));
    let died = wire.io.apply_batch(&cut, &mut |_, _| true);
    wire.fixture.cut_apply_when(None);
    assert!(died.is_err(), "{died:?}");
    assert!(wire.fixture.worktree.join("cut/0.md").exists());
    assert!(!wire.fixture.worktree.join("cut/2.md").exists());
    let next = writes_only(small_writes(1, "next"));
    let outcome = wire
        .restarted()
        .apply_batch(&next, &mut |_, _| true)
        .unwrap();
    assert!(outcome.writes.iter().all(|write| applied(&write.outcome)));
    for index in 0..3 {
        assert!(wire
            .fixture
            .worktree
            .join(format!("cut/{index}.md"))
            .exists());
    }
    assert!(wire.fixture.worktree.join("next/0.md").exists());
    assert_eq!(std::fs::read_dir(wire.journal.path()).unwrap().count(), 0);
}

#[test]
fn an_apply_deadline_grows_with_its_batch() {
    use crate::mcp::remote_destination::apply_wait;
    assert_eq!(apply_wait(0, 0), BATCH_CALL_TIMEOUT);
    assert!(apply_wait(20_000, 0) >= BATCH_CALL_TIMEOUT + Duration::from_secs(400));
    assert!(
        apply_wait(0, 2 * 1024 * 1024 * 1024) >= BATCH_CALL_TIMEOUT + Duration::from_secs(2048)
    );
}

fn exclude_text(wire: &Wire) -> String {
    std::fs::read_to_string(wire.fixture.repo.join(".git/info/exclude")).unwrap_or_default()
}
fn exclude(paths: &[&str], expected: Vec<String>) -> Option<BatchExclude> {
    Some(BatchExclude {
        operation_id: uuid::Uuid::new_v4(),
        workspace_id: "ws-wire".into(),
        expected,
        paths: paths.iter().map(|path| path.to_string()).collect(),
        repair: false,
    })
}

/// Must-fix 2: a write never sent (too large) is not excluded, unless it
/// already was.
#[test]
fn a_too_large_write_is_left_out_of_the_exclude_block() {
    let wire = Wire::current();
    let mut batch = writes_only(small_writes(1, "fit"));
    batch.writes.push(BatchWrite {
        operation_id: uuid::Uuid::new_v4(),
        path: relative("big/huge.bin"),
        bytes: vec![1; codesense_pty_core::remote_files::MAX_FILE_BYTES + 1],
        mode: 0o644,
        expected: ExpectedDestination::Absent,
    });
    batch.exclude = exclude(&["fit/0.md", "big/huge.bin"], vec![]);
    let outcome = wire.io.apply_batch(&batch, &mut |_, _| true).unwrap();
    let text = exclude_text(&wire);
    assert!(text.contains("/fit/0.md"), "{text}");
    assert!(!text.contains("huge.bin"), "{text}");
    assert_eq!(outcome.exclude_paths, Some(vec!["fit/0.md".to_string()]));
}

/// Must-fix 3: a batch cut mid-Apply is reconciled BEFORE the next manifest;
/// the caller learns what it applied and the block it left, and the next
/// batch's compare-and-swap against that block succeeds.
#[test]
fn a_cut_batch_is_reconciled_before_the_next_manifest_and_its_block_is_the_next_expected() {
    let wire = Wire::current();
    let mut cut = writes_only(small_writes(3, "cut"));
    cut.exclude = exclude(&["cut/0.md", "cut/1.md", "cut/2.md"], vec![]);
    wire.fixture
        .cut_apply_when(Some(wire.fixture.worktree.join("cut/0.md")));
    assert!(wire.io.apply_batch(&cut, &mut |_, _| true).is_err());
    wire.fixture.cut_apply_when(None);
    // A fresh process reads the workspace: the cut batch is settled first.
    let io = wire.restarted();
    let manifest = io.manifest(&prefixes(&["cut"]), 100, &mut |_| {}).unwrap();
    let recovered = manifest.recovered.expect("the cut batch was reconciled");
    assert_eq!(recovered.batch_id, cut.batch_id);
    assert_eq!(recovered.phase, TransferPhase::Finished);
    assert!(recovered.writes.iter().all(|write| applied(&write.outcome)));
    assert_eq!(manifest.files.len(), 3, "the diff sees the resumed writes");
    let left = recovered.exclude_paths.expect("its block's final paths");
    assert_eq!(left, vec!["cut/0.md", "cut/1.md", "cut/2.md"]);
    // The next batch's CAS against that block succeeds.
    let mut next = writes_only(small_writes(1, "next"));
    next.exclude = exclude(&["cut/0.md", "cut/1.md", "cut/2.md", "next/0.md"], left);
    let outcome = io.apply_batch(&next, &mut |_, _| true).unwrap();
    assert!(
        outcome
            .operations
            .iter()
            .all(|operation| applied(&operation.outcome)),
        "{:?}",
        outcome.operations
    );
    assert!(
        io.reconcile_pending().unwrap().is_none(),
        "nothing left behind"
    );
}

/// SW-XS-7 Step 2 over the production client and session-server handlers.
fn prepare_over_wire(
    wire: &Wire,
    home: &std::path::Path,
    want: &crate::agents::prepare::WantSet,
) -> crate::agents::prepare::PrepareReport {
    let legacy = || Ok(Vec::new());
    crate::agents::prepare::prepare(crate::agents::prepare::PrepareRequest {
        home,
        principal: &crate::domain::Principal::from(41u64),
        destination: crate::agents::prepare::Destination::Remote {
            io: wire.io.as_ref(),
        },
        want,
        origin: "step2:o/t/barb".into(),
        workspace: "fixture".into(),
        agent: "Barb".into(),
        legacy: &legacy,
        prune: false,
        progress: &|_| {},
        validate: &|| Ok(()),
        explicit: Default::default(),
    })
    .unwrap()
}

/// SW-XS-7 AC1 (counted at the wire, SPEC §11.14): with 2,000 declared files
/// already in place and nothing changed, an agent's Step 2 makes ONE control
/// call (the manifest; budget ≤ 3). The first preparation is one manifest
/// plus one batch. AC7: `git status` in the machine's worktree shows none of
/// the prepared files.
#[test]
fn xs7_step_two_with_two_thousand_unchanged_files_is_one_control_call() {
    let wire = Wire::current();
    let home = tempfile::tempdir().unwrap();
    let content = tempfile::tempdir().unwrap();
    for index in 0..2_000 {
        let prefix = [".claude/skills", "docs"][index % 2];
        let path = content
            .path()
            .join(format!("{prefix}/d{}/f{index}.md", index % 13));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("context {index}")).unwrap();
    }
    let want = crate::agents::prepare::WantSet::from_subtrees(
        content.path(),
        &[".claude/skills", "docs"],
        "team:o/t",
    );
    let first = prepare_over_wire(&wire, home.path(), &want);
    assert_eq!(first.written.len(), 2_000);
    let first_calls = wire.calls();
    assert_eq!(first_calls[0], "workspace_manifest");
    assert_eq!(
        first_calls
            .iter()
            .filter(|kind| *kind == "workspace_manifest")
            .count(),
        1
    );
    assert!(
        !first_calls.iter().any(|kind| kind == "workspace_file"),
        "no per-file call: {first_calls:?}"
    );
    std::thread::sleep(Duration::from_millis(120));
    // The first run's batch is settled (under load its Finish can be left to
    // the next call): the counted window holds only the second run's calls.
    wire.io.reconcile_pending().unwrap();
    let before = wire.calls().len();
    let second = prepare_over_wire(&wire, home.path(), &want);
    assert!(second.written.is_empty());
    let calls: Vec<String> = wire.calls().split_off(before);
    assert!(calls.len() <= 3, "{calls:?}");
    assert_eq!(calls, vec!["workspace_manifest"]);
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(&wire.fixture.worktree)
        .output()
        .unwrap();
    let status = String::from_utf8(status.stdout).unwrap();
    assert!(
        !status.contains(".claude/skills") && !status.contains("docs/"),
        "X7: prepared files show as changes: {status}"
    );
    // MUST 3 at the wire: drop this workspace's block from the clone's shared
    // exclude; the next launch (no file changed) restores it with one manifest
    // and one batch, and nothing shows in git status again.
    std::fs::write(wire.fixture.repo.join(".git/info/exclude"), "# user line\n").unwrap();
    let before = wire.calls().len();
    let third = prepare_over_wire(&wire, home.path(), &want);
    assert!(third.written.is_empty());
    let calls: Vec<String> = wire.calls().split_off(before);
    assert_eq!(calls[0], "workspace_manifest");
    assert_eq!(
        calls.iter().filter(|c| *c == "workspace_manifest").count(),
        1
    );
    assert!(exclude_text(&wire).starts_with("# user line\n"));
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(&wire.fixture.worktree)
        .output()
        .unwrap();
    let status = String::from_utf8(status.stdout).unwrap();
    assert!(
        !status.contains(".claude/skills") && !status.contains("docs/"),
        "restored block hides the prepared files again: {status}"
    );
    assert!(exclude_text(&wire).contains("/docs/d1/f1.md"));
}

/// r2: a terminal server from before `ManifestWorkspaceExcludeV1` (#1975
/// era: `ContextManifestV1` only, whose `ManifestRequest` denies unknown
/// fields) is never sent `workspaceId`: its manifest answers, without a
/// block, and Step 2 still prepares (falling back to its own record).
#[test]
fn xs7_a_server_without_the_exclude_capability_is_not_sent_the_field() {
    let wire = Wire::new(vec![
        Capability::Terminals,
        Capability::SelectedContextBatchV1,
        Capability::ContextManifestV1,
    ]);
    wire.write("docs/a.md", b"a");
    let manifest = wire
        .io
        .manifest(&prefixes(&["docs"]), 100, &mut |_| {})
        .unwrap();
    assert_eq!(manifest.workspace_exclude, None, "the field was not sent");
    // (That the request then carries no `workspaceId` at all is pinned in
    // codesense-pty-core's `remote_manifest` tests.)
    let current = Wire::current();
    current.write("docs/a.md", b"a");
    let reported = current
        .io
        .manifest(&prefixes(&["docs"]), 100, &mut |_| {})
        .unwrap();
    assert!(
        reported.workspace_exclude.is_some(),
        "a current host reports the block"
    );

    let home = tempfile::tempdir().unwrap();
    let content = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(content.path().join("docs")).unwrap();
    std::fs::write(content.path().join("docs/b.md"), b"b").unwrap();
    let want =
        crate::agents::prepare::WantSet::from_subtrees(content.path(), &["docs"], "team:o/t");
    let report = prepare_over_wire(&wire, home.path(), &want);
    assert_eq!(report.written, vec!["docs/b.md"]);
    assert!(exclude_text(&wire).contains("/docs/b.md"));
}

// ------------------------------------------------------------------ SW-XS-8

/// SW-XS-8: a home whose team (`acme/api` / `backend`) holds `files` files
/// under `docs/` plus the fixture's skills.
struct Team {
    home: tempfile::TempDir,
    root: std::path::PathBuf,
}
impl Team {
    fn new(files: usize) -> Self {
        let home = tempfile::tempdir().unwrap();
        let root = crate::agents::resolve::tests::team_content(
            home.path(),
            &crate::agents::resolve::tests::alice(),
        );
        for index in 0..files {
            let path = root.join(format!("docs/d{}/f{index}.md", index % 17));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("doc {index}")).unwrap();
        }
        Self { home, root }
    }
    fn target(&self) -> crate::agents::ManifestTarget {
        crate::agents::ManifestTarget::team(
            crate::agents::resolve::tests::ORG,
            crate::agents::resolve::tests::TEAM,
        )
    }
    fn want(&self, sets: &[&str]) -> crate::agents::prepare::WantSet {
        crate::agents::prepare::step3::want_for_selection(
            self.home.path(),
            &crate::agents::resolve::tests::alice(),
            &self.target(),
            &[],
            &sets.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .unwrap()
    }
    fn files(&self) -> std::collections::BTreeMap<String, Vec<u8>> {
        crate::agents::prepare::step3::tests::snapshot(&self.root)
    }
}

/// An explicit hydrate over the production client and handlers.
fn hydrate_over_wire(
    wire: &Wire,
    team: &Team,
    want: &crate::agents::prepare::WantSet,
    cancel: Option<&dyn Fn() -> bool>,
    progress: &dyn Fn(&crate::agents::prepare::PrepareProgress),
) -> crate::agents::prepare::PrepareReport {
    let legacy = || Ok(Vec::new());
    crate::agents::prepare::step3::hydrate(crate::agents::prepare::step3::Hydrate {
        home: team.home.path(),
        principal: &crate::agents::resolve::tests::alice(),
        destination: crate::agents::prepare::Destination::Remote {
            io: wire.io.as_ref(),
        },
        want,
        operation: "op-wire",
        workspace: "fixture".into(),
        agent: "Barb".into(),
        prune: false,
        overwrite_modified: false,
        progress,
        validate: &|| Ok(()),
        cancel,
        legacy: &legacy,
    })
    .unwrap()
}

fn publish_request<'a>(
    wire: &'a Wire,
    team: &'a Team,
    target: &'a crate::agents::ManifestTarget,
    expect: &'a std::collections::BTreeMap<String, crate::agents::prepare::publish::Expect>,
) -> crate::agents::prepare::publish::PublishRequest<'a> {
    static PRINCIPAL: std::sync::LazyLock<crate::domain::Principal> =
        std::sync::LazyLock::new(crate::agents::resolve::tests::alice);
    crate::agents::prepare::publish::PublishRequest {
        home: team.home.path(),
        principal: &PRINCIPAL,
        destination: crate::agents::prepare::Destination::Remote {
            io: wire.io.as_ref(),
        },
        target,
        paths: &[],
        sets: &[],
        to: None,
        expect,
        include_deletes: false,
        legacy: None,
        operation: "pub-wire",
        workspace: "fixture".into(),
        agent: "Barb".into(),
        progress: &|_| {},
        validate: &|| Ok(()),
        cancel: None,
    }
}

fn git_status(wire: &Wire) -> String {
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(&wire.fixture.worktree)
        .output()
        .unwrap();
    String::from_utf8(status.stdout).unwrap()
}

/// SW-XS-8 AC1 + AC2 (counted at the wire, SPEC §11.14): a machine
/// workspace holds 2,000 hydrated files and 20 of them changed there.
/// - The preview is ONE control call, lists exactly those 20 (each with its
///   target path) and writes nothing.
/// - `reverse_sync_worktree` makes at most 4 control calls (here: the
///   manifest, then one manifest that streams the 20 files back) and team
///   content gets exactly the 20 changes.
#[test]
fn xs8_reverse_sync_twenty_of_two_thousand_is_at_most_four_control_calls() {
    let wire = Wire::current();
    let team = Team::new(2_000);
    let want = team.want(&["docs"]);
    let hydrated = hydrate_over_wire(&wire, &team, &want, None, &|_| {});
    assert_eq!(hydrated.written.len(), 2_000);
    let mut changed: Vec<String> = Vec::new();
    for index in (0..2_000).step_by(100) {
        let path = format!("docs/d{}/f{index}.md", index % 17);
        wire.write(&path, format!("machine edit {index}").as_bytes());
        changed.push(path);
    }
    changed.sort();
    let target = team.target();
    let expect = std::collections::BTreeMap::new();
    let before_team = team.files();
    // The setup's batch is settled (under load its Finish can be left to
    // the next call): the counted window holds only the operation's calls.
    wire.io.reconcile_pending().unwrap();

    let before = wire.calls().len();
    let preview =
        crate::agents::prepare::publish::preview(&publish_request(&wire, &team, &target, &expect))
            .unwrap();
    let calls: Vec<String> = wire.calls().split_off(before);
    assert_eq!(calls, vec!["workspace_manifest"], "AC2: one call");
    let listed: Vec<String> = preview.entries.iter().map(|e| e.path.clone()).collect();
    assert_eq!(listed, changed, "AC2: the same 20");
    for entry in &preview.entries {
        assert_eq!(entry.target_path, team.root.join(&entry.path));
    }
    assert_eq!(team.files(), before_team, "AC2: the preview writes nothing");

    let before = wire.calls().len();
    let report =
        crate::agents::prepare::publish::publish(&publish_request(&wire, &team, &target, &expect))
            .unwrap();
    let calls: Vec<String> = wire.calls().split_off(before);
    assert!(calls.len() <= 4, "AC1: {calls:?}");
    assert!(
        !calls.iter().any(|c| c == "workspace_file"),
        "no per-file call: {calls:?}"
    );
    assert_eq!(
        report.status,
        crate::agents::prepare::publish::PublishStatus::Published,
        "{report:?}"
    );
    let after = team.files();
    let mut differs: Vec<String> = after
        .keys()
        .filter(|path| before_team.get(*path) != after.get(*path))
        .cloned()
        .collect();
    differs.sort();
    assert_eq!(differs, changed, "AC1: exactly the 20 changes");
    for path in &changed {
        let index: usize = path
            .rsplit_once("/f")
            .unwrap()
            .1
            .trim_end_matches(".md")
            .parse()
            .unwrap();
        assert_eq!(after[path], format!("machine edit {index}").as_bytes());
    }
}

/// SW-XS-8 AC1 with new files (review r1): publishing 20 edits plus 5 new
/// files is the two manifest calls plus ONE exclude-block batch that keeps
/// the new files hidden from git (begin, seal, apply, finish) — the
/// documented bound — and never a per-file call. Team content gets exactly
/// the 20 changes and the 5 new files.
#[test]
fn xs8_reverse_sync_with_new_files_adds_one_exclude_batch_and_nothing_else() {
    let wire = Wire::current();
    let team = Team::new(2_000);
    let want = team.want(&["docs"]);
    let hydrated = hydrate_over_wire(&wire, &team, &want, None, &|_| {});
    assert_eq!(hydrated.written.len(), 2_000);
    let mut changed: Vec<String> = Vec::new();
    for index in (0..2_000).step_by(100) {
        let path = format!("docs/d{}/f{index}.md", index % 17);
        wire.write(&path, format!("machine edit {index}").as_bytes());
        changed.push(path);
    }
    for index in 0..5 {
        let path = format!("docs/new/n{index}.md");
        wire.write(&path, format!("new {index}").as_bytes());
        changed.push(path);
    }
    changed.sort();
    let target = team.target();
    let expect = std::collections::BTreeMap::new();
    let before_team = team.files();
    wire.io.reconcile_pending().unwrap();
    let before = wire.calls().len();
    let report =
        crate::agents::prepare::publish::publish(&publish_request(&wire, &team, &target, &expect))
            .unwrap();
    let calls: Vec<String> = wire.calls().split_off(before);
    assert_eq!(
        calls.iter().filter(|c| *c == "workspace_manifest").count(),
        2,
        "{calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|c| *c == "batch_apply").count(),
        1,
        "one exclude batch: {calls:?}"
    );
    assert!(calls.len() <= 2 + 4, "the documented bound: {calls:?}");
    assert!(!calls.iter().any(|c| c == "workspace_file"), "{calls:?}");
    assert_eq!(
        report.status,
        crate::agents::prepare::publish::PublishStatus::Published,
        "{report:?}"
    );
    let after = team.files();
    let mut differs: Vec<String> = after
        .keys()
        .filter(|path| before_team.get(*path) != after.get(*path))
        .cloned()
        .collect();
    differs.sort();
    assert_eq!(differs, changed);
    assert_eq!(git_status(&wire), "", "X7: the new files stay hidden");
}

/// SW-XS-8 AC3 + X7 on a machine: a team skill edit, then
/// `hydrate_worktree {sets:[".claude/skills"]}` transfers only that file,
/// in one batch whose single Apply also carries the exclude block (no
/// separate exclude call), and `git status --porcelain` stays empty.
#[test]
fn xs8_hydrate_sets_after_a_skill_edit_transfers_only_that_file() {
    let wire = Wire::current();
    let team = Team::new(0);
    let first = hydrate_over_wire(&wire, &team, &team.want(&[".claude/skills"]), None, &|_| {});
    assert_eq!(first.written.len(), 2, "{first:?}");
    assert_eq!(git_status(&wire), "", "X7 after a Step 3 add");
    assert!(exclude_text(&wire).contains("/.claude/skills/bmad-build/SKILL.md"));
    std::fs::write(
        team.root.join(".claude/skills/bmad-build/SKILL.md"),
        b"# Build v2\n",
    )
    .unwrap();
    let before = wire.calls().len();
    let again = hydrate_over_wire(&wire, &team, &team.want(&[".claude/skills"]), None, &|_| {});
    assert_eq!(again.written, vec![".claude/skills/bmad-build/SKILL.md"]);
    let calls: Vec<String> = wire.calls().split_off(before);
    assert_eq!(
        calls.iter().filter(|c| *c == "batch_apply").count(),
        1,
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c == "workspace_file"), "{calls:?}");
    assert_eq!(
        std::fs::read(
            wire.fixture
                .worktree
                .join(".claude/skills/bmad-build/SKILL.md")
        )
        .unwrap(),
        b"# Build v2\n"
    );
    assert_eq!(git_status(&wire), "");
}

/// SW-XS-8 AC5 on a machine: cancelled mid-transfer (after the first chunk),
/// the batch is abandoned before its Apply: no file — partial or whole — is
/// visible in the workspace, and the result is `cancelled`.
#[test]
fn xs8_a_cancelled_remote_hydrate_applies_nothing() {
    let wire = Wire::current();
    let team = Team::new(0);
    for index in 0u8..3 {
        // Incompressible: the batch may gzip.
        let mut bytes = vec![0u8; 400_000];
        blake3::Hasher::new()
            .update(&[index])
            .finalize_xof()
            .fill(&mut bytes);
        let path = team.root.join(format!("big/f{index}.bin"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let sent = std::sync::atomic::AtomicU64::new(0);
    let cancel = || sent.load(std::sync::atomic::Ordering::SeqCst) > 0;
    let progress = |p: &crate::agents::prepare::PrepareProgress| {
        if p.phase == crate::agents::prepare::PreparePhase::Transfer && p.bytes > 0 {
            sent.store(p.bytes, std::sync::atomic::Ordering::SeqCst);
        }
    };
    let report = hydrate_over_wire(&wire, &team, &team.want(&["big"]), Some(&cancel), &progress);
    assert_eq!(
        report.status,
        crate::agents::prepare::PrepareStatus::Cancelled
    );
    assert!(report.written.is_empty());
    let calls = wire.calls();
    assert!(calls.iter().any(|c| c == "batch_chunk"), "{calls:?}");
    assert!(!calls.iter().any(|c| c == "batch_apply"), "{calls:?}");
    assert!(
        calls.iter().any(|c| c == "batch_finish"),
        "abandoned: {calls:?}"
    );
    assert!(!wire.fixture.worktree.join("big").exists());
    assert_eq!(git_status(&wire), "");
    assert!(
        wire.io.reconcile_pending().unwrap().is_none(),
        "nothing left behind"
    );
}

/// SW-XS-8: a terminal server without `ManifestReadV1` can preview, but a
/// publish that needs file bytes is refused with the update message before
/// any byte is read: there is no per-file fallback.
#[test]
fn xs8_publish_on_an_old_server_asks_for_an_update() {
    let wire = Wire::new(vec![
        Capability::Terminals,
        Capability::SelectedContextBatchV1,
        Capability::ContextManifestV1,
        Capability::ManifestWorkspaceExcludeV1,
    ]);
    let team = Team::new(3);
    hydrate_over_wire(&wire, &team, &team.want(&["docs"]), None, &|_| {});
    wire.write("docs/d0/f0.md", b"edited");
    let target = team.target();
    let expect = std::collections::BTreeMap::new();
    let error =
        crate::agents::prepare::publish::publish(&publish_request(&wire, &team, &target, &expect))
            .unwrap_err();
    assert!(
        DestinationBatchError::names_outdated_server(&error),
        "{error}"
    );
    assert!(!wire.calls().iter().any(|c| c == "workspace_file"));
    assert_eq!(
        std::fs::read(team.root.join("docs/d0/f0.md")).unwrap(),
        b"doc 0"
    );
}

/// SW-XS-8 X6 on a machine: after a machine-side edit (and a new file), the
/// scope-less form of `hydrate_worktree` and a selected one leave team
/// content byte-identical; the edit is kept on the machine.
#[test]
fn xs8_remote_hydrate_never_touches_team_content() {
    let wire = Wire::current();
    let team = Team::new(3);
    let docs = team.want(&["docs"]);
    hydrate_over_wire(&wire, &team, &docs, None, &|_| {});
    wire.write("docs/d0/f0.md", b"machine edit");
    wire.write("docs/d0/new.md", b"machine new file");
    let before = team.files();
    let scope_less = crate::agents::prepare::step3::want_for_selection(
        team.home.path(),
        &crate::agents::resolve::tests::alice(),
        &team.target(),
        &[],
        &[],
    )
    .unwrap();
    let selected = crate::agents::prepare::step3::want_for_selection(
        team.home.path(),
        &crate::agents::resolve::tests::alice(),
        &team.target(),
        &["docs/d0/f0.md".to_string()],
        &[],
    )
    .unwrap();
    for want in [&scope_less, &selected, &docs] {
        let report = hydrate_over_wire(&wire, &team, want, None, &|_| {});
        assert!(!report.written.contains(&"docs/d0/f0.md".to_string()));
    }
    assert_eq!(team.files(), before, "X6: hydrate never publishes");
    assert_eq!(
        std::fs::read(wire.fixture.worktree.join("docs/d0/f0.md")).unwrap(),
        b"machine edit"
    );
}

// ------------------------------------------------------------------ SW-XS-12

// SW-XS-12: the §9 budget table (X2: O(1) control round trips plus streamed
// data, whatever the file count), counted at the client's request-id dispatch.
// Each operation runs at two sizes, 10x apart: its control calls must be the
// same sequence at both (they do not grow with the files), within the row's
// budget; the streamed chunks may grow only with the bytes. (The 2,000-file
// rows themselves are pinned by the SW-XS-7/8 tests above.) An Apply that
// outlives one call's deadline is re-asked (idempotently) — a wall-clock
// effect a loaded machine's fsyncs can trigger — so these tests lift that
// deadline: they count the protocol's calls, not the machine's speed.

/// The control calls in `calls`: everything but the streamed data chunks.
fn control(calls: &[String]) -> Vec<String> {
    calls
        .iter()
        .filter(|c| *c != "batch_chunk")
        .cloned()
        .collect()
}

/// Streamed chunks in `calls`.
fn chunks(calls: &[String]) -> usize {
    calls.iter().filter(|c| *c == "batch_chunk").count()
}

/// The most chunks `bytes` of file content may take: the bytes over the
/// chunk size, plus one for the batch's framing.
fn chunk_bound(bytes: usize) -> usize {
    bytes.div_ceil(codesense_pty_core::context_batch::CHUNK_BYTES) + 1
}

/// The calls one step makes, after the previous step's batch is settled.
fn counted(wire: &Wire, step: impl FnOnce()) -> Vec<String> {
    APPLY_WAIT_OVERRIDE.with(|f| f.set(Some((Duration::from_secs(900), Duration::from_secs(900)))));
    wire.io.reconcile_pending().unwrap();
    let before = wire.calls().len();
    step();
    wire.calls().split_off(before)
}

/// §9 rows 1 and 2: Step 2 on a machine. First time: one manifest plus one
/// streamed batch, the same control calls for 100 files as for 1,000. Nothing
/// changed: one manifest, within the budget of 3 — at both sizes.
#[test]
fn xs12_budget_step_two_on_a_machine_is_constant_in_the_file_count() {
    let mut first_time = Vec::new();
    let mut no_op = Vec::new();
    for files in [100, 1_000] {
        let wire = Wire::current();
        let home = tempfile::tempdir().unwrap();
        let content = tempfile::tempdir().unwrap();
        let mut bytes = 0;
        for index in 0..files {
            let path = content
                .path()
                .join(format!("docs/d{}/f{index}.md", index % 13));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let body = format!("context {index}");
            bytes += body.len();
            std::fs::write(path, body).unwrap();
        }
        let want =
            crate::agents::prepare::WantSet::from_subtrees(content.path(), &["docs"], "team:o/t");
        let calls = counted(&wire, || {
            assert_eq!(
                prepare_over_wire(&wire, home.path(), &want).written.len(),
                files
            );
        });
        assert!(
            chunks(&calls) <= chunk_bound(bytes),
            "{files} files: {calls:?}"
        );
        let control_calls = control(&calls);
        assert_eq!(
            control_calls
                .iter()
                .filter(|c| *c == "workspace_manifest")
                .count(),
            1,
            "{control_calls:?}"
        );
        assert_eq!(
            control_calls.iter().filter(|c| *c == "batch_apply").count(),
            1,
            "one streamed batch, one Apply: {control_calls:?}"
        );
        assert!(
            !control_calls.iter().any(|c| c == "workspace_file"),
            "{control_calls:?}"
        );
        first_time.push(control_calls);
        std::thread::sleep(Duration::from_millis(120));
        let calls = counted(&wire, || {
            assert!(prepare_over_wire(&wire, home.path(), &want)
                .written
                .is_empty());
        });
        assert!(
            calls.len() <= 3,
            "§9 budget: ≤ 3 control calls, {files} files: {calls:?}"
        );
        no_op.push(calls);
    }
    assert_eq!(
        first_time[0], first_time[1],
        "first time: constant in the file count"
    );
    assert_eq!(
        no_op[0], no_op[1],
        "nothing changed: constant in the file count"
    );
    assert_eq!(no_op[1], vec!["workspace_manifest"]);
}

/// Explicit `hydrate_worktree` on a machine (SW-XS-8): the same shape as a
/// launch — one manifest plus one streamed batch the first time, one manifest
/// when nothing changed — whatever the file count.
#[test]
fn xs12_budget_hydrate_on_a_machine_is_constant_in_the_file_count() {
    let mut first_time = Vec::new();
    let mut no_op = Vec::new();
    for files in [100, 1_000] {
        let wire = Wire::current();
        let team = Team::new(files);
        let want = team.want(&["docs"]);
        let calls = counted(&wire, || {
            assert!(
                hydrate_over_wire(&wire, &team, &want, None, &|_| {})
                    .written
                    .len()
                    >= files
            );
        });
        let control_calls = control(&calls);
        assert_eq!(
            control_calls
                .iter()
                .filter(|c| *c == "workspace_manifest")
                .count(),
            1,
            "{control_calls:?}"
        );
        assert_eq!(
            control_calls.iter().filter(|c| *c == "batch_apply").count(),
            1,
            "{control_calls:?}"
        );
        assert!(
            !control_calls.iter().any(|c| c == "workspace_file"),
            "{control_calls:?}"
        );
        first_time.push(control_calls);
        std::thread::sleep(Duration::from_millis(120));
        let calls = counted(&wire, || {
            assert!(hydrate_over_wire(&wire, &team, &want, None, &|_| {})
                .written
                .is_empty());
        });
        assert!(calls.len() <= 3, "{files} files: {calls:?}");
        no_op.push(calls);
    }
    assert_eq!(
        first_time[0], first_time[1],
        "first time: constant in the file count"
    );
    assert_eq!(
        no_op[0], no_op[1],
        "nothing changed: constant in the file count"
    );
}

/// §9 row 4: `reverse_sync_worktree` on a machine, 1,000 files hydrated:
/// publishing 10 changes and publishing 100 make the same control calls,
/// within the budget of 4, and never a per-file call.
#[test]
fn xs12_budget_reverse_sync_is_constant_in_the_changed_count() {
    let mut per_size = Vec::new();
    for changed_every in [100, 10] {
        let wire = Wire::current();
        let team = Team::new(1_000);
        let want = team.want(&["docs"]);
        APPLY_WAIT_OVERRIDE
            .with(|f| f.set(Some((Duration::from_secs(900), Duration::from_secs(900)))));
        assert_eq!(
            hydrate_over_wire(&wire, &team, &want, None, &|_| {})
                .written
                .len(),
            1_000
        );
        let mut changed = 0;
        for index in (0..1_000).step_by(changed_every) {
            wire.write(
                &format!("docs/d{}/f{index}.md", index % 17),
                format!("edit {index}").as_bytes(),
            );
            changed += 1;
        }
        let target = team.target();
        let expect = std::collections::BTreeMap::new();
        let calls = counted(&wire, || {
            let report = crate::agents::prepare::publish::publish(&publish_request(
                &wire, &team, &target, &expect,
            ))
            .unwrap();
            assert_eq!(
                report.status,
                crate::agents::prepare::publish::PublishStatus::Published
            );
        });
        assert!(
            calls.len() <= 4,
            "§9 budget: ≤ 4 control calls, {changed} changed: {calls:?}"
        );
        assert!(!calls.iter().any(|c| c == "workspace_file"), "{calls:?}");
        per_size.push(calls);
    }
    assert_eq!(
        per_size[0], per_size[1],
        "10 changed and 100 changed: the same calls"
    );
}

// ----------------------------------------------------------------- SW-XS-26

/// SW-XS-26 AC5: a host advertising `ContextBatchZstdV1` receives the batch
/// as one zstd frame (smaller on the wire, applied byte for byte); a host
/// without it receives identity, as before; an incompressible batch goes as
/// identity even to a zstd host.
#[test]
fn xs26_the_batch_travels_compressed_only_to_a_host_that_decodes_it() {
    use codesense_pty_core::context_batch::Codec;
    let text: Vec<u8> = (0..400)
        .flat_map(|line| {
            format!("- line {line} of agent context, the same words again\n").into_bytes()
        })
        .collect();
    let noise = |mut seed: u32| -> Vec<u8> {
        (0..64 * 1024)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect()
    };
    let mut zstd_host = Wire::current_capabilities();
    zstd_host.push(Capability::ContextBatchZstdV1);
    for (capabilities, files, expected) in [
        (zstd_host.clone(), [text.clone(), text.clone()], Codec::Zstd),
        (
            Wire::current_capabilities(),
            [text.clone(), text.clone()],
            Codec::Identity,
        ),
        (
            zstd_host,
            [noise(0x2545_f491), noise(0x9e37_79b9)],
            Codec::Identity,
        ),
    ] {
        let wire = Wire::new(capabilities);
        let home = tempfile::tempdir().unwrap();
        let content = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(content.path().join("docs")).unwrap();
        std::fs::write(content.path().join("docs/a.md"), &files[0]).unwrap();
        std::fs::write(content.path().join("docs/b.md"), &files[1]).unwrap();
        let want =
            crate::agents::prepare::WantSet::from_subtrees(content.path(), &["docs"], "team:o/t");
        let report = prepare_over_wire(&wire, home.path(), &want);
        assert_eq!(report.written, vec!["docs/a.md", "docs/b.md"]);
        for (name, bytes) in ["docs/a.md", "docs/b.md"].iter().zip(&files) {
            assert_eq!(
                &std::fs::read(wire.fixture.worktree.join(name)).unwrap(),
                bytes,
                "{expected:?}"
            );
        }
        let begins = wire.begins.lock().unwrap().clone();
        assert_eq!(begins.len(), 1);
        let (codec, raw, encoded) = begins[0];
        assert_eq!(codec, expected);
        assert_eq!(raw, (files[0].len() + files[1].len()) as u64);
        match codec {
            Codec::Zstd => assert!(encoded * 4 < raw, "{encoded} of {raw}"),
            _ => assert_eq!(encoded, raw),
        }
    }
}

/// SW-XS-26 review r2 L8: a host that refuses the zstd frame at Seal gets the
/// same batch once more, uncompressed, and it applies.
#[test]
fn xs26_a_refused_zstd_seal_falls_back_to_identity_once() {
    use codesense_pty_core::context_batch::Codec;
    let mut capabilities = Wire::current_capabilities();
    capabilities.push(Capability::ContextBatchZstdV1);
    let wire = Wire::new(capabilities);
    wire.refuse_zstd_seal.store(true, Ordering::SeqCst);
    let text: Vec<u8> = (0..400)
        .flat_map(|line| format!("- line {line} of agent context, again\n").into_bytes())
        .collect();
    let home = tempfile::tempdir().unwrap();
    let content = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(content.path().join("docs")).unwrap();
    std::fs::write(content.path().join("docs/a.md"), &text).unwrap();
    let want =
        crate::agents::prepare::WantSet::from_subtrees(content.path(), &["docs"], "team:o/t");
    let report = prepare_over_wire(&wire, home.path(), &want);
    assert_eq!(report.written, vec!["docs/a.md"]);
    assert_eq!(
        std::fs::read(wire.fixture.worktree.join("docs/a.md")).unwrap(),
        text
    );
    let codecs: Vec<Codec> = wire
        .begins
        .lock()
        .unwrap()
        .iter()
        .map(|begin| begin.0)
        .collect();
    assert_eq!(codecs, vec![Codec::Zstd, Codec::Identity]);
}

/// SW-XS-26 review r2 L7: a libzstd frame larger than its 4 MiB window (so
/// it carries a window descriptor, not a single segment) passes the host's
/// header check and applies byte for byte.
#[test]
fn xs26_a_multi_window_zstd_frame_applies() {
    use codesense_pty_core::context_batch::Codec;
    let mut capabilities = Wire::current_capabilities();
    capabilities.push(Capability::ContextBatchZstdV1);
    let wire = Wire::new(capabilities);
    let home = tempfile::tempdir().unwrap();
    let content = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(content.path().join("docs")).unwrap();
    let mut seed = 0x9e37_79b9u32;
    let mut files = Vec::new();
    for index in 0..5 {
        // 1.5 MB of text-like bytes per file, 7.5 MB in all.
        let bytes: Vec<u8> = (0..1_500_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                b"abcdefgh ijkl\n"[(seed % 14) as usize]
            })
            .collect();
        std::fs::write(content.path().join(format!("docs/{index}.md")), &bytes).unwrap();
        files.push(bytes);
    }
    let want =
        crate::agents::prepare::WantSet::from_subtrees(content.path(), &["docs"], "team:o/t");
    let report = prepare_over_wire(&wire, home.path(), &want);
    assert_eq!(report.written.len(), 5);
    for (index, bytes) in files.iter().enumerate() {
        assert_eq!(
            &std::fs::read(wire.fixture.worktree.join(format!("docs/{index}.md"))).unwrap(),
            bytes
        );
    }
    let begins = wire.begins.lock().unwrap().clone();
    assert_eq!(begins.len(), 1);
    assert_eq!(begins[0].0, Codec::Zstd);
    assert!(begins[0].1 > 4 * 1024 * 1024);
}

// ------------------------------------------------------------------ SW-XS-27

fn count_over_wire(
    team: &Team,
    io: &dyn DestinationIo,
    target: &crate::agents::ManifestTarget,
) -> crate::agents::prepare::edits::WorkspaceEdits {
    crate::agents::prepare::edits::count(&crate::agents::prepare::edits::CountRequest {
        home: team.home.path(),
        principal: &crate::agents::resolve::tests::alice(),
        destination: crate::agents::prepare::Destination::Remote { io },
        target,
        legacy: None,
    })
}

fn journal_files(wire: &Wire) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(wire.journal.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

/// SW-XS-27 AC6 (counted at the wire, SPEC §11.14): counting a machine
/// workspace's unpublished team-file edits is exactly ONE manifest call and
/// no batch call, lists the edited and new files, and writes nothing — not
/// team content, not the workspace, not the journal.
#[test]
fn xs27_a_remote_count_is_one_manifest_call_and_writes_nothing() {
    use crate::agents::prepare::edits::{EditKind, EditsState};
    let wire = Wire::current();
    let team = Team::new(200);
    let want = team.want(&["docs"]);
    hydrate_over_wire(&wire, &team, &want, None, &|_| {});
    wire.write("docs/d0/f0.md", b"machine edit 0");
    wire.write("docs/d3/f20.md", b"machine edit 20");
    wire.write("docs/new/n0.md", b"new on the machine");
    // The setup's batch is settled: the counted window holds only the count.
    wire.io.reconcile_pending().unwrap();
    let target = team.target();
    let team_before = team.files();
    let journal_before = journal_files(&wire);
    let status_before = git_status(&wire);

    let before = wire.calls().len();
    let edits = count_over_wire(&team, wire.io.as_ref(), &target);
    let calls: Vec<String> = wire.calls().split_off(before);
    assert_eq!(calls, vec!["workspace_manifest"], "one read-only call");
    assert_eq!(edits.state, EditsState::Checked, "{edits:?}");
    let listed: Vec<(&str, EditKind)> = edits
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.kind))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("docs/d0/f0.md", EditKind::Changed),
            ("docs/d3/f20.md", EditKind::Changed),
            ("docs/new/n0.md", EditKind::New),
        ]
    );
    assert!(
        edits.files.iter().all(|f| f.changed_at_ms.is_none()),
        "a machine reports no file times"
    );
    assert_eq!(team.files(), team_before, "team content untouched");
    assert_eq!(journal_files(&wire), journal_before, "journal untouched");
    assert_eq!(git_status(&wire), status_before, "workspace untouched");
}

/// SW-XS-27 AC6: with a batch an earlier call left behind (journaled), a
/// count makes NO call — finishing that batch would write on the machine —
/// and says so; the batch is still there for the next explicit operation.
#[test]
fn xs27_a_count_never_finishes_an_earlier_cut_copy() {
    use crate::agents::prepare::edits::EditsState;
    let wire = Wire::current();
    let team = Team::new(10);
    let want = team.want(&["docs"]);
    hydrate_over_wire(&wire, &team, &want, None, &|_| {});
    wire.io.reconcile_pending().unwrap();
    let cut = writes_only(small_writes(3, "cut"));
    wire.fixture
        .cut_apply_when(Some(wire.fixture.worktree.join("cut/0.md")));
    assert!(wire.io.apply_batch(&cut, &mut |_, _| true).is_err());
    wire.fixture.cut_apply_when(None);
    let journal_before = journal_files(&wire);
    assert_eq!(journal_before.len(), 1, "the cut batch is journaled");
    let target = team.target();

    let io = wire.restarted();
    let before = wire.calls().len();
    let edits = count_over_wire(&team, io.as_ref(), &target);
    let calls: Vec<String> = wire.calls().split_off(before);
    assert!(calls.is_empty(), "no call at all: {calls:?}");
    assert_eq!(edits.state, EditsState::UnfinishedCopy);
    assert_eq!(journal_files(&wire), journal_before, "still pending");
    assert!(
        !wire.fixture.worktree.join("cut/2.md").exists(),
        "the cut batch was not resumed"
    );
    // The read-only manifest refuses too, rather than resuming it.
    assert!(io
        .manifest_observe(&prefixes(&["docs"]), 100, &mut |_| {})
        .is_err());
    assert!(wire.calls().split_off(before).is_empty());
}

/// SW-XS-27: a journal that can't be read is still a copy left behind: the
/// count makes no call and reports it, rather than reading it as nothing.
#[test]
fn xs27_an_unreadable_journal_is_still_an_unfinished_copy() {
    use crate::agents::prepare::edits::EditsState;
    let wire = Wire::current();
    let team = Team::new(5);
    let want = team.want(&["docs"]);
    hydrate_over_wire(&wire, &team, &want, None, &|_| {});
    wire.io.reconcile_pending().unwrap();
    let cut = writes_only(small_writes(1, "cut"));
    wire.fixture
        .cut_apply_when(Some(wire.fixture.worktree.join("cut/0.md")));
    assert!(wire.io.apply_batch(&cut, &mut |_, _| true).is_err());
    wire.fixture.cut_apply_when(None);
    let (name, _) = journal_files(&wire).pop().expect("journaled");
    std::fs::write(wire.journal.path().join(&name), b"{ torn").unwrap();
    let target = team.target();
    let before = wire.calls().len();
    let edits = count_over_wire(&team, wire.restarted().as_ref(), &target);
    assert!(wire.calls().split_off(before).is_empty());
    assert_eq!(edits.state, EditsState::UnfinishedCopy);
}

/// SW-XS-27: Discard of a new file on a machine is one batch of removals, each
/// compare-and-swapped against the bytes the plan saw; team content and copied
/// files are never touched.
#[test]
fn xs27_discarding_a_new_file_on_a_machine_removes_exactly_it() {
    let wire = Wire::current();
    let team = Team::new(5);
    let want = team.want(&["docs"]);
    hydrate_over_wire(&wire, &team, &want, None, &|_| {});
    wire.write("docs/new/n0.md", b"new on the machine");
    wire.write("docs/new/n1.md", b"also new");
    wire.io.reconcile_pending().unwrap();
    let target = team.target();
    let expect = std::collections::BTreeMap::new();
    let team_before = team.files();
    let paths = vec!["docs/new/n0.md".to_string()];
    let mut request = publish_request(&wire, &team, &target, &expect);
    request.paths = &paths;
    let before = wire.calls().len();
    let done = crate::agents::prepare::publish::discard_new_files(&request).unwrap();
    let calls: Vec<String> = wire.calls().split_off(before);
    assert_eq!(done.removed, paths, "{done:?}");
    assert!(done.failed.is_empty());
    assert!(!wire.fixture.worktree.join("docs/new/n0.md").exists());
    assert!(wire.fixture.worktree.join("docs/new/n1.md").exists());
    assert_eq!(team.files(), team_before, "team content untouched");
    assert_eq!(
        calls.iter().filter(|c| *c == "workspace_manifest").count(),
        1,
        "{calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|c| *c == "batch_apply").count(),
        1,
        "{calls:?}"
    );
    // A copied file is refused, before anything is sent.
    let copied = vec!["docs/d0/f0.md".to_string()];
    request.paths = &copied;
    let before = wire.calls().len();
    assert!(crate::agents::prepare::publish::discard_new_files(&request).is_err());
    assert!(
        !wire
            .calls()
            .split_off(before)
            .iter()
            .any(|c| c.starts_with("batch_")),
        "nothing written"
    );
}

#[test]
fn missing_workspace_count_crosses_the_wire() {
    use crate::agents::prepare::edits::EditsState;
    let wire = Wire::current();
    let team = Team::new(2);
    let want = team.want(&["docs"]);
    hydrate_over_wire(&wire, &team, &want, None, &|_| {});
    wire.io.reconcile_pending().unwrap();
    std::fs::remove_dir_all(&wire.fixture.worktree).unwrap();
    let before = wire.calls().len();
    let edits = count_over_wire(&team, wire.io.as_ref(), &team.target());
    assert_eq!(edits.state, EditsState::Missing, "{edits:?}");
    assert!(edits.files.is_empty());
    assert_eq!(
        edits.reason.as_deref(),
        Some("folder no longer exists on the machine")
    );
    assert_eq!(&wire.calls()[before..], &["workspace_manifest"]);
}

#[test]
fn forget_missing_workspace_is_capability_gated_and_marker_only_over_wire() {
    use codesense_pty_core::remote_workspace::ForgetMissingWorkspace;
    let old = Wire::current();
    let before = old.client.rm4f_snapshot().requests.len();
    let error = old
        .client
        .forget_missing_workspace(ForgetMissingWorkspace {
            target: old.fixture.target.clone(),
        })
        .unwrap_err();
    assert!(DestinationBatchError::names_outdated_server(&error));
    assert_eq!(
        old.client.rm4f_snapshot().requests.len(),
        before,
        "an old host receives no cleanup request"
    );
    let mut caps = Wire::current_capabilities();
    caps.push(Capability::ForgetMissingWorkspaceV1);
    let wire = Wire::new(caps);
    let request = ForgetMissingWorkspace {
        target: wire.fixture.target.clone(),
    };
    assert!(wire
        .client
        .forget_missing_workspace(request.clone())
        .is_err());
    assert!(wire.fixture.worktree.is_dir());
    std::fs::remove_dir_all(&wire.fixture.worktree).unwrap();
    wire.client.forget_missing_workspace(request).unwrap();
    assert!(!wire.fixture.worktree.exists());
    assert!(wire.fixture.repo.is_dir());
}

#[test]
fn forgetting_retires_the_old_record_before_reprepared_workspace_can_report_deletions() {
    use crate::agents::prepare::{edits::EditsState, ManagedRecord};
    let mut caps = Wire::current_capabilities();
    caps.push(Capability::ForgetMissingWorkspaceV1);
    let wire = Wire::new(caps);
    let team = Team::new(2);
    let want = team.want(&["docs"]);
    hydrate_over_wire(&wire, &team, &want, None, &|_| {});
    wire.io.reconcile_pending().unwrap();
    let team_before = team.files();
    let principal = crate::agents::resolve::tests::alice();
    let identity = wire.io.identity().clone();
    assert!(ManagedRecord::exists(
        team.home.path(),
        &principal,
        &identity
    ));
    let cleanup = || {
        wire.client.forget_missing_workspace(
            codesense_pty_core::remote_workspace::ForgetMissingWorkspace {
                target: wire.fixture.target.clone(),
            },
        )
    };
    assert!(crate::mcp::team_file_edits::forget_missing_record(
        team.home.path(),
        &principal,
        &identity,
        cleanup
    )
    .is_err());
    assert!(
        ManagedRecord::exists(team.home.path(), &principal, &identity),
        "refusal keeps the record"
    );
    std::fs::remove_dir_all(&wire.fixture.worktree).unwrap();
    crate::mcp::team_file_edits::forget_missing_record(
        team.home.path(),
        &principal,
        &identity,
        cleanup,
    )
    .unwrap();
    assert!(!ManagedRecord::exists(
        team.home.path(),
        &principal,
        &identity
    ));
    let target = wire.fixture.prepare_again();
    assert_eq!(target.handle, wire.fixture.target.handle);
    let DestinationIdentity::Remote(remote) = identity else {
        panic!("remote fixture")
    };
    let recreated = crate::mcp::remote_destination::from_admitted_for_test(
        remote,
        target,
        wire.client.clone(),
        Arc::new(|| Ok(())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(Mutex::new(None)),
    );
    let edits = count_over_wire(&team, recreated.as_ref(), &team.target());
    assert_eq!(edits.state, EditsState::Checked, "{edits:?}");
    assert_eq!(
        edits.counts.removed, 0,
        "the new workspace has no old user deletions"
    );
    assert!(edits.files.is_empty());
    assert_eq!(
        team.files(),
        team_before,
        "canonical team content is unchanged"
    );
}

#[test]
fn a_prepared_workspace_without_copied_files_is_still_checked_for_absence() {
    use crate::agents::prepare::edits::EditsState;
    let wire = Wire::current();
    let team = Team::new(0);
    assert_eq!(
        count_over_wire(&team, wire.io.as_ref(), &team.target()).state,
        EditsState::Checked
    );
    std::fs::remove_dir_all(&wire.fixture.worktree).unwrap();
    let before = wire.calls().len();
    let found = count_over_wire(&team, wire.io.as_ref(), &team.target());
    assert_eq!(found.state, EditsState::Missing, "{found:?}");
    assert_eq!(&wire.calls()[before..], &["workspace_manifest"]);
}
