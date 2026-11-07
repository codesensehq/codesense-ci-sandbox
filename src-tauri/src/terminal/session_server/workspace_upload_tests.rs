// SPDX-License-Identifier: MIT
//! Exercises production dispatch, serialization and the bounded writer with a
//! synthetic framed peer. Real Git bytes are used; this is not SSH evidence.
use super::*;

#[test]
fn remote_workspace_multi_mib_writer_resumes_original_bundle_after_unknown_upload() {
    use crate::mcp::remote_workspace::dispatch;
    use crate::mcp::remote_workspace_bundle::{build, BuildSource};
    use codesense_pty_core::remote_workspace::{
        BundleProgress, PrepareWorkspace, PreparedWorkspace,
    };
    use sha2::{Digest, Sha256};

    let root = short_root();
    let seed = root.path().join("seed");
    let repo = git2::Repository::init(&seed).unwrap();
    let url = "https://github.com/fixture/workspace-writer.git";
    repo.remote("origin", url).unwrap();
    let repository_key = crate::context_repo::canonical_remote_key(url).unwrap();
    let mut random = 17u32;
    let data: Vec<u8> = (0..3 * 1024 * 1024)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random as u8
        })
        .collect();
    let mut tree = repo.treebuilder(None).unwrap();
    tree.insert("tracked.bin", repo.blob(&data).unwrap(), 0o100644)
        .unwrap();
    let tree = repo.find_tree(tree.write().unwrap()).unwrap();
    let sig = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
    repo.commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let request_id = uuid::Uuid::new_v4().to_string();
    let (path, bundle) = build(
        root.path(),
        &request_id,
        &request_id,
        BuildSource {
            url: url.into(),
            repository_key: repository_key.clone(),
            base_ref: "main".into(),
            seed: Some(seed),
        },
        || true,
        |_| Ok(Default::default()),
    )
    .unwrap();
    assert!(bundle.size > 3 * 1024 * 1024);
    let expected_bytes = std::fs::read(&path).unwrap();
    let request = PrepareWorkspace {
        request_id,
        owner_id: 41,
        origin_key: "a".repeat(64),
        repository_key,
        clone_url: url.into(),
        repo_path: "/remote/repo".into(),
        worktree_path: "/remote/work".into(),
        base_ref: "main".into(),
        branch: "work".into(),
        bundle,
    };
    let original = request.clone();
    let ((client_reader, client_writer), (mut host_reader, mut host_writer)) =
        memory_transport_pair();
    let host = std::thread::spawn(move || {
        let mut received = Vec::new();
        let mut chunks = 0;
        let mut probes = Vec::new();
        let mut prepared = None;
        while let Some(frame) = read_frame(&mut host_reader).unwrap() {
            assert!(frame.encode().len() <= WRITE_QUEUE_BYTES);
            let Message::Call { rid, request } = frame.decode_json().unwrap() else {
                panic!("expected call")
            };
            let reply = match request {
                Request::Listen => Reply::Done,
                Request::ReconcileRemoteWorkspace { request } => {
                    assert_eq!(request, original);
                    Reply::RemoteWorkspaceReceipt {
                        result: prepared.clone(),
                    }
                }
                Request::UploadWorkspaceBundle { chunk } => {
                    assert_eq!(
                        chunk.request, original,
                        "every attempt keeps the immutable intent"
                    );
                    if chunk.bytes.is_empty() {
                        assert_eq!(chunk.offset, 0);
                        probes.push(received.len());
                    } else {
                        assert_eq!(
                            chunk.offset as usize,
                            received.len(),
                            "resume only at acknowledged peer offset"
                        );
                        received.extend_from_slice(&chunk.bytes);
                        chunks += 1;
                    }
                    if chunks == 1 && !chunk.bytes.is_empty() {
                        // Persisted bytes, but no usable upload acknowledgement.
                        // Avoid a wall-clock timeout while exercising the same unknown outcome.
                        Reply::Done
                    } else {
                        Reply::WorkspaceBundleProgress {
                            progress: BundleProgress {
                                next_offset: received.len() as u64,
                            },
                        }
                    }
                }
                Request::PrepareRemoteWorkspace { request } => {
                    assert_eq!(request, original);
                    assert_eq!(received, expected_bytes);
                    assert_eq!(
                        format!("{:x}", Sha256::digest(&received)),
                        original.bundle.digest
                    );
                    assert!(
                        chunks > 3,
                        "multi-chunk transfer must traverse the production writer"
                    );
                    assert_eq!(probes.len(), 2);
                    assert_eq!(probes[0], 0);
                    assert!(probes[1] > 0, "unknown upload resumes retained staging");
                    let result = Box::new(PreparedWorkspace {
                        status: "prepared".into(),
                        handle: Some("fixture".into()),
                        cwd: Some(original.worktree_path.clone()),
                        repo_path: Some(original.repo_path.clone()),
                        repository_key: Some(original.repository_key.clone()),
                        resolved_commit: Some(original.bundle.commit.clone()),
                        branch: Some(original.branch.clone()),
                        message: None,
                    });
                    let mut receipt = result.clone();
                    receipt.status = "alreadyPrepared".into();
                    prepared = Some(receipt);
                    Reply::RemoteWorkspacePrepared { result }
                }
                other => panic!("unexpected request {other:?}"),
            };
            write_frame(
                &mut host_writer,
                &Frame::control(&Message::Reply {
                    rid,
                    result: Ok(reply),
                })
                .unwrap(),
            )
            .unwrap();
        }
    });
    let interrupted = client_writer.0.clone();
    let mut transport = SessionServerTransport::new(client_reader, client_writer, move || {
        interrupted.closed.store(true, Ordering::SeqCst);
        interrupted.ready.notify_all();
    });
    let (lease, _revoke) = crate::commands::auth::private_authority::test_lease(41);
    transport.private_lease = Some(lease);
    let client = SessionServerClient::from_admitted_transport(
        root.path(),
        AdmittedSessionTransport {
            transport,
            hello: mock_hello(instance_id(root.path())),
            remote_close: None,
        },
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    let unknown = dispatch(&client, request.clone(), &path, || Ok(())).unwrap();
    assert_eq!(unknown.status, "unknown");
    assert_eq!(
        unknown.message.as_deref(),
        Some("unexpected bundle progress reply")
    );
    assert!(path.exists(), "unknown result retains original bytes");
    let prepared = dispatch(&client, request.clone(), &path, || Ok(())).unwrap();
    assert_eq!(prepared.status, "prepared", "{prepared:?}");
    assert!(prepared.message.is_none(), "{prepared:?}");
    assert!(
        !path.exists(),
        "only confirmed preparation settles local bytes"
    );
    let reconciled = dispatch(&client, request, &path, || Ok(())).unwrap();
    assert_eq!(reconciled.status, "alreadyPrepared");
    assert_eq!(reconciled.handle, prepared.handle);
    client.writer.close();
    host.join().unwrap();
}

#[test]
fn workspace_file_large_apply_preserves_bytes_and_recovery_operation_id() {
    use codesense_pty_core::remote_files::{
        Effect, EffectOutcome, Expected, FileOperation, FileReply, FileRequest, FileTarget,
        Observation, MAX_FILE_BYTES,
    };
    use sha2::{Digest, Sha256};

    let root = short_root();
    let target = FileTarget {
        owner_id: 41,
        origin_key: "a".repeat(64),
        request_id: uuid::Uuid::new_v4().to_string(),
        handle: "remote-workspace-large-write-fixture".into(),
    };
    // The live failing size and the file protocol's full supported limit.
    let sizes = [1_100_013, MAX_FILE_BYTES];
    let operation_ids = sizes.map(|_| uuid::Uuid::new_v4().to_string());
    let host_ids = operation_ids.clone();
    let host_target = target.clone();
    let (client_socket, mut host_socket) = UnixStream::pair().unwrap();
    for socket in [&client_socket, &host_socket] {
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
    }
    let host = std::thread::spawn(move || {
        let listen = read_frame(&mut host_socket).unwrap().unwrap();
        let Message::Call {
            rid,
            request: Request::Listen,
        } = listen.decode_json().unwrap()
        else {
            panic!("expected initial Listen call")
        };
        write_frame(
            &mut host_socket,
            &Frame::control(&Message::Reply {
                rid,
                result: Ok(Reply::Done),
            })
            .unwrap(),
        )
        .unwrap();
        for (size, operation_id) in sizes.into_iter().zip(host_ids) {
            // An intact ledger with no receipt admits only this same operation
            // to retry. This peer models that reply; real ledger tests own its
            // authority checks.
            for reconcile in [true, false] {
                let frame = read_frame(&mut host_socket).unwrap().unwrap();
                let Message::Call {
                    rid,
                    request: Request::WorkspaceFile { request },
                } = frame.decode_json().unwrap()
                else {
                    panic!("expected WorkspaceFile call")
                };
                assert_eq!(request.target, host_target);
                let outcome = if reconcile {
                    assert_eq!(
                        request.operation,
                        FileOperation::Reconcile {
                            operation_id: operation_id.clone()
                        }
                    );
                    EffectOutcome::NotStarted {
                        operation_id: operation_id.clone(),
                    }
                } else {
                    assert!(frame.encode().len() > 1024 * 1024);
                    assert!(frame.encode().len() <= WRITE_QUEUE_BYTES);
                    let FileOperation::Apply {
                        operation_id: actual_id,
                        path,
                        expected,
                        effect: Effect::Write { bytes, mode },
                    } = request.operation
                    else {
                        panic!("expected file write")
                    };
                    assert_eq!(actual_id, operation_id);
                    assert_eq!(path, "nested/font-index.json");
                    assert_eq!(expected, Expected::Absent);
                    assert_eq!(mode, Some(0o644));
                    assert_eq!(bytes.len(), size);
                    assert!(bytes
                        .iter()
                        .enumerate()
                        .all(|(i, byte)| *byte == (i % 251) as u8));
                    EffectOutcome::Applied {
                        observation: Observation::Regular {
                            hash: Sha256::digest(&bytes).into(),
                            mtime_ns: 1,
                            size: size as u64,
                            mode: 0o644,
                        },
                    }
                };
                write_frame(
                    &mut host_socket,
                    &Frame::control(&Message::Reply {
                        rid,
                        result: Ok(Reply::WorkspaceFile {
                            result: Box::new(FileReply::Effect { outcome }),
                        }),
                    })
                    .unwrap(),
                )
                .unwrap();
            }
        }
    });
    let interrupt_socket = client_socket.try_clone().unwrap();
    let mut transport = SessionServerTransport::new(
        client_socket.try_clone().unwrap(),
        client_socket,
        move || {
            let _ = interrupt_socket.shutdown(std::net::Shutdown::Both);
        },
    );
    let (lease, _revoke) = crate::commands::auth::private_authority::test_lease(41);
    transport.private_lease = Some(lease);
    let client = SessionServerClient::from_admitted_transport(
        root.path(),
        AdmittedSessionTransport {
            transport,
            hello: mock_hello(instance_id(root.path())),
            remote_close: None,
        },
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap();
    for (size, operation_id) in sizes.into_iter().zip(operation_ids) {
        assert_eq!(
            client
                .workspace_file(FileRequest {
                    target: target.clone(),
                    operation: FileOperation::Reconcile {
                        operation_id: operation_id.clone()
                    },
                })
                .unwrap(),
            FileReply::Effect {
                outcome: EffectOutcome::NotStarted {
                    operation_id: operation_id.clone()
                }
            }
        );
        let bytes: Vec<_> = (0..size).map(|i| (i % 251) as u8).collect();
        let hash = Sha256::digest(&bytes).into();
        let reply = client
            .workspace_file(FileRequest {
                target: target.clone(),
                operation: FileOperation::Apply {
                    operation_id,
                    path: "nested/font-index.json".into(),
                    expected: Expected::Absent,
                    effect: Effect::Write {
                        bytes,
                        mode: Some(0o644),
                    },
                },
            })
            .unwrap();
        assert_eq!(
            reply,
            FileReply::Effect {
                outcome: EffectOutcome::Applied {
                    observation: Observation::Regular {
                        hash,
                        mtime_ns: 1,
                        size: size as u64,
                        mode: 0o644,
                    }
                }
            }
        );
    }
    client.writer.close();
    host.join().unwrap();
}

/// SW-XS-9: the pipelined upload over the production client and writer:
/// up to 8 chunk calls in flight on one connection, acked in reverse order,
/// resumed from the machine's bitmap, then one seal.
#[test]
fn xs9_pipelined_chunks_ride_one_connection_with_out_of_order_acks() {
    use crate::mcp::remote_workspace::{transfer, PrepareProgress};
    use codesense_pty_core::remote_workspace::{
        BundleUploadStatus, GitBundle, PrepareWorkspace, ReceivedChunks, PIPELINED_CHUNK_BYTES,
    };
    use sha2::{Digest, Sha256};

    let root = short_root();
    let mut random = 5u32;
    let bytes: Vec<u8> = (0..20 * PIPELINED_CHUNK_BYTES + 77)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random as u8
        })
        .collect();
    let path = root.path().join("objects.bundle");
    std::fs::write(&path, &bytes).unwrap();
    let request = PrepareWorkspace {
        request_id: uuid::Uuid::new_v4().to_string(),
        owner_id: 41,
        origin_key: "a".repeat(64),
        repository_key: "github.com/fixture/pipe".into(),
        clone_url: "https://github.com/fixture/pipe.git".into(),
        repo_path: "/remote/repo".into(),
        worktree_path: "/remote/work".into(),
        base_ref: "main".into(),
        branch: "work".into(),
        bundle: GitBundle {
            size: bytes.len() as u64,
            digest: format!("{:x}", Sha256::digest(&bytes)),
            commit: "c".repeat(40),
        },
    };
    let size = bytes.len() as u64;
    let expected = bytes.clone();
    let ((client_reader, client_writer), (mut host_reader, mut host_writer)) =
        memory_transport_pair();
    let host = std::thread::spawn(move || {
        let mut held = ReceivedChunks::for_size(size);
        // An interrupted earlier attempt left chunks 0 and 7.
        held.insert(0);
        held.insert(7);
        let mut staged: Vec<u8> = vec![0; size as usize];
        for start in [0, 7 * PIPELINED_CHUNK_BYTES] {
            staged[start..start + PIPELINED_CHUNK_BYTES]
                .copy_from_slice(&expected[start..start + PIPELINED_CHUNK_BYTES]);
        }
        let mut buffered: Vec<(u64, u32)> = Vec::new();
        let mut max_buffered = 0;
        let mut sent_indices = Vec::new();
        let mut sealed = 0;
        let reply = |writer: &mut MemoryWriter, rid: u64, reply: Reply| {
            write_frame(
                writer,
                &Frame::control(&Message::Reply {
                    rid,
                    result: Ok(reply),
                })
                .unwrap(),
            )
            .unwrap();
        };
        while let Some(frame) = read_frame(&mut host_reader).unwrap() {
            let Message::Call { rid, request } = frame.decode_json().unwrap() else {
                panic!("expected call")
            };
            match request {
                Request::Listen => reply(&mut host_writer, rid, Reply::Done),
                Request::BundleUploadStatus { .. } => reply(
                    &mut host_writer,
                    rid,
                    Reply::BundleUploadStatus {
                        status: BundleUploadStatus {
                            chunk_bytes: PIPELINED_CHUNK_BYTES as u32,
                            received: held.clone(),
                            sealed: false,
                        },
                    },
                ),
                Request::UploadBundleChunk { chunk } => {
                    assert_eq!(
                        chunk.sha256,
                        format!("{:x}", Sha256::digest(&chunk.bytes)),
                        "each chunk carries its own hash"
                    );
                    let start = chunk.index as usize * PIPELINED_CHUNK_BYTES;
                    staged[start..start + chunk.bytes.len()].copy_from_slice(&chunk.bytes);
                    sent_indices.push(chunk.index);
                    buffered.push((rid, chunk.index));
                    max_buffered = max_buffered.max(buffered.len());
                    // Every chunk not yet acked is in flight once the window is full.
                    if buffered.len() == held.missing().len().min(8) {
                        // Answer the whole window, newest first.
                        for (rid, index) in buffered.drain(..).rev() {
                            held.insert(index);
                            reply(&mut host_writer, rid, Reply::BundleChunkStaged { index });
                        }
                    }
                }
                Request::SealWorkspaceBundle { .. } => {
                    assert!(held.is_complete());
                    assert_eq!(staged, expected);
                    sealed += 1;
                    reply(&mut host_writer, rid, Reply::Done);
                }
                other => panic!("unexpected request {other:?}"),
            }
        }
        (max_buffered, sent_indices, sealed)
    });
    let interrupted = client_writer.0.clone();
    let mut transport = SessionServerTransport::new(client_reader, client_writer, move || {
        interrupted.closed.store(true, Ordering::SeqCst);
        interrupted.ready.notify_all();
    });
    let (lease, _revoke) = crate::commands::auth::private_authority::test_lease(41);
    transport.private_lease = Some(lease);
    let mut hello = mock_hello(instance_id(root.path()));
    hello.capabilities.push(Capability::BundlePipelineV1);
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
    let events = std::sync::Mutex::new(Vec::new());
    transfer(client.as_ref(), &request, &path, &|| Ok(()), &|event| {
        events.lock().unwrap().push(event)
    })
    .unwrap();
    client.writer.close();
    let (max_buffered, mut sent_indices, sealed) = host.join().unwrap();
    assert_eq!(max_buffered, 8, "eight chunk calls in flight at once");
    sent_indices.sort_unstable();
    let mut missing: Vec<u32> = (0..21).filter(|i| *i != 0 && *i != 7).collect();
    missing.sort_unstable();
    assert_eq!(sent_indices, missing, "only the chunks the machine lacks");
    assert_eq!(sealed, 1);
    let events = events.into_inner().unwrap();
    assert_eq!(
        events.first(),
        Some(&PrepareProgress::Copying {
            sent: 2 * PIPELINED_CHUNK_BYTES as u64,
            total: size
        })
    );
    assert!(events.contains(&PrepareProgress::Copying {
        sent: size,
        total: size
    }));
    assert_eq!(events.last(), Some(&PrepareProgress::Verifying));
}
