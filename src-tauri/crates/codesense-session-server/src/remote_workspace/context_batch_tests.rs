// SPDX-License-Identifier: MIT
use super::*;
use crate::remote_workspace::tests::{fixture, prepared};
use codesense_pty_core::remote_files::Expected;

/// One zstd frame recording its content size, as the desktop's
/// libzstd encoder writes it. ruzstd's encoder may leave the size out, so its
/// header is re-laid (RFC 8878 section 3.1.1) with an 8-byte content size.
pub(crate) fn zstd_encode(raw: &[u8]) -> Vec<u8> {
    let frame = ruzstd::encoding::compress_to_vec(raw, ruzstd::encoding::CompressionLevel::Fastest);
    let descriptor = frame[4];
    let single_segment = descriptor & 0x20 != 0;
    let window = if single_segment { 0 } else { 1 };
    let dictionary = [0, 1, 2, 4][usize::from(descriptor & 0x03)];
    let content = match descriptor >> 6 {
        0 if single_segment => 1,
        0 => 0,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let header = 5 + window + dictionary + content;
    // An 8-byte content size, no dictionary, the frame's own segment layout.
    let mut out = frame[..4].to_vec();
    out.push((descriptor & 0x24) | 0xC0);
    out.extend_from_slice(&frame[5..5 + window]);
    out.extend_from_slice(&(raw.len() as u64).to_le_bytes());
    out.extend_from_slice(&frame[header..]);
    out
}
pub(crate) fn fixture_batch(codec: Codec) -> (tempfile::TempDir, String, BatchRequest, Vec<u8>) {
    let (root, setup) = fixture();
    let prepared = prepared(root.path(), setup.clone());
    assert_eq!(prepared.status, "prepared", "{prepared:?}");
    let raw = b"first selected filesecond selected file".to_vec();
    let first = b"first selected file";
    let second = b"second selected file";
    let encoded = match codec {
        Codec::Identity => raw.clone(),
        Codec::Gzip => {
            let mut encoder = flate2::GzBuilder::new()
                .mtime(0)
                .write(Vec::new(), flate2::Compression::default());
            encoder.write_all(&raw).unwrap();
            encoder.finish().unwrap()
        }
        Codec::Zstd => zstd_encode(&raw),
    };
    let items = [first.as_slice(), second.as_slice()]
        .iter()
        .enumerate()
        .map(|(index, bytes)| BatchItem {
            index,
            operation_id: uuid::Uuid::new_v4().to_string(),
            path: format!("nested/context/{index}.md"),
            offset: if index == 0 { 0 } else { first.len() as u64 },
            length: bytes.len() as u64,
            hash: *blake3::hash(bytes).as_bytes(),
            mode: 0o640,
            expected: Expected::Absent,
            dependencies: if index == 0 { vec![] } else { vec![0] },
        })
        .collect();
    let manifest = BatchManifest {
        request_id: uuid::Uuid::new_v4().to_string(),
        generation: 1,
        codec,
        raw_length: raw.len() as u64,
        encoded_length: encoded.len() as u64,
        raw_digest: *blake3::hash(&raw).as_bytes(),
        encoded_digest: *blake3::hash(&encoded).as_bytes(),
        items,
        removes: vec![],
        chmods: vec![],
        ensure_dirs: vec![],
        exclude_block: None,
    };
    let request = BatchRequest {
        version: VERSION,
        target: FileTarget {
            owner_id: 41,
            origin_key: setup.origin_key,
            request_id: setup.request_id,
            handle: prepared.handle.unwrap(),
        },
        batch_id: uuid::Uuid::new_v4().to_string(),
        manifest_digest: manifest.digest().unwrap(),
        operation: BatchOperation::Begin { manifest },
    };
    (root, setup.worktree_path, request, encoded)
}
pub(crate) fn call(
    root: &Path,
    request: &BatchRequest,
    operation: BatchOperation,
) -> Result<TransferStatus, String> {
    let reply = handle(
        root,
        41,
        BatchRequest {
            operation,
            ..request.clone()
        },
        || true,
    )?;
    let BatchReply::Transfer { status } = reply else {
        panic!("unexpected inspection")
    };
    Ok(status)
}
pub(crate) fn upload(root: &Path, request: &BatchRequest, encoded: &[u8]) {
    assert_eq!(
        call(root, request, request.operation.clone())
            .unwrap()
            .offset,
        0
    );
    for (index, chunk) in encoded.chunks(CHUNK_BYTES).enumerate() {
        call(
            root,
            request,
            BatchOperation::Chunk {
                offset: (index * CHUNK_BYTES) as u64,
                bytes: chunk.to_vec(),
            },
        )
        .unwrap();
    }
    assert_eq!(
        call(root, request, BatchOperation::Seal).unwrap().phase,
        TransferPhase::Verified
    );
}
#[test]
fn identity_and_gzip_apply_original_members_and_repeat_without_rewrite() {
    for codec in [Codec::Identity, Codec::Gzip, Codec::Zstd] {
        let (root, cwd, request, encoded) = fixture_batch(codec);
        std::fs::write(Path::new(&cwd).join("dirty.txt"), b"leave dirty").unwrap();
        upload(root.path(), &request, &encoded);
        assert!(!Path::new(&cwd).join("nested").exists());
        let before = call(root.path(), &request, BatchOperation::Status).unwrap();
        assert!(before.items.iter().all(|item| matches!(
            item.outcome,
            MemberOutcome::Effect {
                outcome: EffectOutcome::NotStarted { .. }
            }
        )));
        let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
        assert!(applied.items.iter().all(|item| successful(&item.outcome)));
        let BatchOperation::Begin { manifest } = &request.operation else {
            unreachable!()
        };
        for (item, result) in manifest.items.iter().zip(&applied.items) {
            assert_eq!(item.operation_id, result.operation_id);
            assert_eq!(item.index, result.index);
            let path = Path::new(&cwd).join(&item.path);
            assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
            let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
            call(root.path(), &request, BatchOperation::Apply).unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().modified().unwrap(),
                modified
            );
            let receipt = files::handle(
                root.path(),
                41,
                FileRequest {
                    target: request.target.clone(),
                    operation: FileOperation::Reconcile {
                        operation_id: item.operation_id.clone(),
                    },
                },
                || true,
            )
            .unwrap();
            assert!(matches!(
                receipt,
                FileReply::Effect {
                    outcome: EffectOutcome::Applied { .. }
                }
            ));
        }
        assert_eq!(
            std::fs::read(Path::new(&cwd).join("nested/context/0.md")).unwrap(),
            b"first selected file"
        );
        assert_eq!(
            std::fs::read(Path::new(&cwd).join("nested/context/1.md")).unwrap(),
            b"second selected file"
        );
        assert_eq!(
            std::fs::read(Path::new(&cwd).join("dirty.txt")).unwrap(),
            b"leave dirty"
        );
        for _ in 0..2 {
            assert_eq!(
                call(
                    root.path(),
                    &request,
                    BatchOperation::Finish {
                        disposition: FinishDisposition::Settled
                    }
                )
                .unwrap()
                .phase,
                TransferPhase::Finished
            );
        }
        assert!(call(root.path(), &request, BatchOperation::Apply).is_err());
    }
}
#[test]
fn upload_offsets_busy_identity_and_abandonment_are_durable() {
    let (root, cwd, request, encoded) = fixture_batch(Codec::Identity);
    call(root.path(), &request, request.operation.clone()).unwrap();
    let first = BatchOperation::Chunk {
        offset: 0,
        bytes: encoded[..8].to_vec(),
    };
    assert_eq!(
        call(root.path(), &request, first.clone()).unwrap().offset,
        8
    );
    assert_eq!(call(root.path(), &request, first).unwrap().offset, 8);
    assert!(call(
        root.path(),
        &request,
        BatchOperation::Chunk {
            offset: 4,
            bytes: encoded[4..12].to_vec()
        }
    )
    .is_err());
    assert!(call(
        root.path(),
        &request,
        BatchOperation::Chunk {
            offset: 0,
            bytes: vec![0; 8]
        }
    )
    .is_err());
    assert!(call(
        root.path(),
        &request,
        BatchOperation::Chunk {
            offset: 9,
            bytes: vec![0]
        }
    )
    .is_err());
    let other = BatchRequest {
        batch_id: uuid::Uuid::new_v4().to_string(),
        ..request.clone()
    };
    assert!(call(root.path(), &other, other.operation.clone())
        .unwrap_err()
        .contains("busy"));
    let mutated = BatchRequest {
        manifest_digest: [1; 32],
        ..request.clone()
    };
    assert!(call(root.path(), &mutated, BatchOperation::Status).is_err());
    assert_eq!(
        call(root.path(), &request, BatchOperation::Status)
            .unwrap()
            .offset,
        8
    );
    assert!(call(root.path(), &request, BatchOperation::Seal).is_err());
    call(
        root.path(),
        &request,
        BatchOperation::Finish {
            disposition: FinishDisposition::Abandon,
        },
    )
    .unwrap();
    assert!(call(root.path(), &request, BatchOperation::Apply).is_err());
    assert!(!Path::new(&cwd).join("nested").exists());
    assert_eq!(
        call(root.path(), &other, other.operation.clone())
            .unwrap()
            .phase,
        TransferPhase::Uploading
    );
}
#[test]
fn gzip_trailing_truncated_or_expanding_payload_has_zero_effects() {
    for fault in 0..3 {
        let (root, cwd, mut request, mut encoded) = fixture_batch(Codec::Gzip);
        if fault == 0 {
            encoded.extend_from_slice(b"trailing");
        }
        if fault == 1 {
            encoded.truncate(encoded.len() - 3);
        }
        let BatchOperation::Begin { manifest } = &mut request.operation else {
            unreachable!()
        };
        manifest.encoded_length = encoded.len() as u64;
        manifest.encoded_digest = *blake3::hash(&encoded).as_bytes();
        if fault == 2 {
            manifest.raw_length -= 1;
            manifest.items[1].length -= 1;
        }
        request.manifest_digest = manifest.digest().unwrap();
        call(root.path(), &request, request.operation.clone()).unwrap();
        call(
            root.path(),
            &request,
            BatchOperation::Chunk {
                offset: 0,
                bytes: encoded,
            },
        )
        .unwrap();
        assert!(call(root.path(), &request, BatchOperation::Seal).is_err());
        assert!(call(root.path(), &request, BatchOperation::Apply).is_err());
        assert!(!Path::new(&cwd).join("nested").exists());
    }
}
#[test]
fn cas_conflict_blocks_dependents_and_revoked_connection_does_not_dispatch() {
    let (root, cwd, request, encoded) = fixture_batch(Codec::Identity);
    upload(root.path(), &request, &encoded);
    assert!(handle(
        root.path(),
        41,
        BatchRequest {
            operation: BatchOperation::Apply,
            ..request.clone()
        },
        || false
    )
    .is_err());
    assert!(!Path::new(&cwd).join("nested").exists());
    std::fs::create_dir_all(Path::new(&cwd).join("nested/context")).unwrap();
    std::fs::write(
        Path::new(&cwd).join("nested/context/0.md"),
        b"concurrent edit",
    )
    .unwrap();
    let status = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert!(matches!(
        status.items[0].outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Conflict { .. }
        }
    ));
    assert!(matches!(
        status.items[1].outcome,
        MemberOutcome::Blocked { .. }
    ));
    assert!(!Path::new(&cwd).join("nested/context/1.md").exists());
    assert_eq!(
        std::fs::read(Path::new(&cwd).join("nested/context/0.md")).unwrap(),
        b"concurrent edit"
    );
    call(
        root.path(),
        &request,
        BatchOperation::Finish {
            disposition: FinishDisposition::Settled,
        },
    )
    .unwrap();
}

#[test]
fn interrupted_reservations_resume_same_identity_without_recreating_uploaded_state() {
    for before_record in [true, false] {
        let (root, cwd, request, encoded) = fixture_batch(Codec::Identity);
        if before_record {
            FAIL_AFTER_RESERVATION.with(|flag| flag.set(true));
        } else {
            FAIL_DURING_RESERVATION.with(|flag| flag.set(true));
        }
        assert!(call(root.path(), &request, request.operation.clone()).is_err());
        assert!(!Path::new(&cwd).join("nested").exists());
        let changed = BatchRequest {
            manifest_digest: [7; 32],
            ..request.clone()
        };
        assert!(call(root.path(), &changed, request.operation.clone()).is_err());
        upload(root.path(), &request, &encoded);
        let status = call(root.path(), &request, BatchOperation::Apply).unwrap();
        assert!(status.items.iter().all(|item| successful(&item.outcome)));
    }
    let (root, _, request, encoded) = fixture_batch(Codec::Identity);
    call(root.path(), &request, request.operation.clone()).unwrap();
    call(
        root.path(),
        &request,
        BatchOperation::Chunk {
            offset: 0,
            bytes: encoded[..8].to_vec(),
        },
    )
    .unwrap();
    let ledger = files::batch_ledger(
        root.path(),
        41,
        &request.target,
        &files::locate(root.path(), 41, &request.target, &|| true).unwrap(),
        &|| true,
    )
    .unwrap();
    let all = directory(&ledger, "context-batches", false).unwrap();
    let key = digest(
        format!(
            "{}:{}:{}",
            41, request.target.origin_key, request.target.handle
        )
        .as_bytes(),
    );
    let workspace = directory(&all, &key, false).unwrap();
    let staging = directory(&workspace, &request.batch_id, false).unwrap();
    let file = payload(&staging, "encoded", encoded.len() as u64, true).unwrap();
    file.set_len(0).unwrap();
    // Already-uploaded state is never re-reserved or silently reconstructed.
    assert_eq!(
        call(root.path(), &request, request.operation.clone())
            .unwrap()
            .offset,
        8
    );
    assert!(call(
        root.path(),
        &request,
        BatchOperation::Chunk {
            offset: 8,
            bytes: encoded[8..].to_vec()
        }
    )
    .is_err());
}

#[test]
fn gzip_second_member_and_checksum_errors_never_seal() {
    for second_member in [true, false] {
        let (root, cwd, mut request, mut encoded) = fixture_batch(Codec::Gzip);
        if second_member {
            encoded.extend_from_within(..);
        } else {
            let at = encoded.len() - 8;
            encoded[at] ^= 1;
        }
        let BatchOperation::Begin { manifest } = &mut request.operation else {
            unreachable!()
        };
        manifest.encoded_length = encoded.len() as u64;
        manifest.encoded_digest = *blake3::hash(&encoded).as_bytes();
        request.manifest_digest = manifest.digest().unwrap();
        call(root.path(), &request, request.operation.clone()).unwrap();
        call(
            root.path(),
            &request,
            BatchOperation::Chunk {
                offset: 0,
                bytes: encoded,
            },
        )
        .unwrap();
        assert!(call(root.path(), &request, BatchOperation::Seal).is_err());
        assert!(!Path::new(&cwd).join("nested").exists());
    }
}

#[test]
fn nested_symlink_conflict_has_recoverable_receipt_without_escape() {
    let (root, cwd, request, encoded) = fixture_batch(Codec::Identity);
    upload(root.path(), &request, &encoded);
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, Path::new(&cwd).join("nested")).unwrap();
    let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert!(matches!(
        applied.items[0].outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Conflict { .. }
        }
    ));
    assert!(matches!(
        applied.items[1].outcome,
        MemberOutcome::Blocked { .. }
    ));
    let status = call(root.path(), &request, BatchOperation::Status).unwrap();
    assert_eq!(status.items, applied.items);
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
    call(
        root.path(),
        &request,
        BatchOperation::Finish {
            disposition: FinishDisposition::Settled,
        },
    )
    .unwrap();
}

#[test]
fn cut_after_first_effect_reconciles_original_ids_before_continuing() {
    let (root, cwd, request, encoded) = fixture_batch(Codec::Identity);
    upload(root.path(), &request, &encoded);
    let first = Path::new(&cwd).join("nested/context/0.md");
    let second = Path::new(&cwd).join("nested/context/1.md");
    assert!(handle(
        root.path(),
        41,
        BatchRequest {
            operation: BatchOperation::Apply,
            ..request.clone()
        },
        || !first.exists()
    )
    .is_err());
    assert!(first.exists());
    assert!(!second.exists());
    let modified = std::fs::metadata(&first).unwrap().modified().unwrap();
    let status = call(root.path(), &request, BatchOperation::Status).unwrap();
    assert!(successful(&status.items[0].outcome));
    assert!(!second.exists());
    assert!(call(
        root.path(),
        &request,
        BatchOperation::Finish {
            disposition: FinishDisposition::Abandon
        }
    )
    .is_err());
    let resumed = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert!(resumed.items.iter().all(|item| successful(&item.outcome)));
    assert_eq!(
        std::fs::metadata(first).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(std::fs::read(second).unwrap(), b"second selected file");
}

#[test]
fn original_uuid_cannot_reuse_another_file_receipt() {
    let (root, cwd, request, encoded) = fixture_batch(Codec::Identity);
    let BatchOperation::Begin { manifest } = &request.operation else {
        unreachable!()
    };
    files::handle(
        root.path(),
        41,
        FileRequest {
            target: request.target.clone(),
            operation: FileOperation::Apply {
                operation_id: manifest.items[0].operation_id.clone(),
                path: "other.txt".into(),
                expected: Expected::Absent,
                effect: Effect::Write {
                    bytes: b"first selected file".to_vec(),
                    mode: Some(0o640),
                },
            },
        },
        || true,
    )
    .unwrap();
    upload(root.path(), &request, &encoded);
    assert!(call(root.path(), &request, BatchOperation::Apply)
        .unwrap_err()
        .contains("immutable file intent"));
    assert!(!Path::new(&cwd).join("nested").exists());
}

/// `InspectSelected` reports a changed item per item instead of
/// failing the whole inspection.
#[test]
fn inspect_selected_reports_a_mismatch_per_item() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    std::fs::write(Path::new(&cwd).join("same.md"), b"unchanged").unwrap();
    std::fs::write(Path::new(&cwd).join("moved.md"), b"edited since the diff").unwrap();
    let reply = handle(
        root.path(),
        41,
        BatchRequest {
            operation: BatchOperation::InspectSelected {
                items: vec![
                    InspectItem {
                        path: "same.md".into(),
                        expected_hash: Some(*blake3::hash(b"unchanged").as_bytes()),
                    },
                    InspectItem {
                        path: "moved.md".into(),
                        expected_hash: Some(*blake3::hash(b"what the diff saw").as_bytes()),
                    },
                ],
            },
            ..request
        },
        || true,
    )
    .expect("one changed item no longer fails the call");
    let BatchReply::Inspected { items } = reply else {
        panic!("expected inspection")
    };
    assert_eq!(items.len(), 2);
    assert!(!items[0].mismatch);
    assert!(items[1].mismatch);
    assert!(matches!(items[1].observation, Observation::Regular { .. }));
}

/// The payload of `members` files of `size` bytes each, as one identity
/// batch of plain writes.
pub(crate) fn large_batch(
    request: &BatchRequest,
    members: usize,
    size: usize,
) -> (BatchRequest, Vec<u8>) {
    let mut raw = Vec::with_capacity(members * size);
    let mut items = Vec::new();
    let mut seed = 0x9e37_79b9u32;
    for index in 0..members {
        let start = raw.len();
        for _ in 0..size {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            raw.push(seed as u8);
        }
        let bytes = &raw[start..];
        items.push(BatchItem {
            index,
            operation_id: uuid::Uuid::new_v4().to_string(),
            path: format!("large/{index}.bin"),
            offset: start as u64,
            length: size as u64,
            hash: *blake3::hash(bytes).as_bytes(),
            mode: 0o644,
            expected: Expected::Absent,
            dependencies: vec![],
        });
    }
    let digest = *blake3::hash(&raw).as_bytes();
    let manifest = BatchManifest {
        request_id: uuid::Uuid::new_v4().to_string(),
        generation: 1,
        codec: Codec::Identity,
        raw_length: raw.len() as u64,
        encoded_length: raw.len() as u64,
        raw_digest: digest,
        encoded_digest: digest,
        items,
        removes: vec![],
        chmods: vec![],
        ensure_dirs: vec![],
        exclude_block: None,
    };
    (
        BatchRequest {
            batch_id: uuid::Uuid::new_v4().to_string(),
            manifest_digest: manifest.digest().unwrap(),
            operation: BatchOperation::Begin { manifest },
            ..request.clone()
        },
        raw,
    )
}

/// A 50 MB transfer of 25 members streams in chunks with no item
/// or byte cap error, and every member lands.
#[test]
fn fifty_megabyte_transfer_streams_without_a_cap_error() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let (request, raw) = large_batch(&request, 25, 2 * 1024 * 1024);
    assert!(raw.len() >= 50 * 1024 * 1024);
    upload(root.path(), &request, &raw);
    let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert_eq!(applied.items.len(), 25);
    assert!(applied.items.iter().all(|item| successful(&item.outcome)));
    assert_eq!(
        std::fs::read(Path::new(&cwd).join("large/24.bin")).unwrap(),
        raw[24 * 2 * 1024 * 1024..]
    );
}

/// More than 64 members in one batch.
#[test]
fn a_batch_of_two_hundred_members_applies_in_one_apply() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let (request, raw) = large_batch(&request, 200, 64);
    upload(root.path(), &request, &raw);
    let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert_eq!(applied.items.len(), 200);
    assert!(applied.items.iter().all(|item| successful(&item.outcome)));
    assert!(Path::new(&cwd).join("large/199.bin").exists());
}

pub(crate) fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub(crate) fn hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}
/// A batch of `writes` (path, bytes, expected) plus the extension lists.
pub(crate) fn mixed_batch(
    request: &BatchRequest,
    writes: &[(String, Vec<u8>, Expected)],
    removes: Vec<RemoveItem>,
    chmods: Vec<ChmodItem>,
    ensure_dirs: Vec<DirItem>,
    exclude_block: Option<Box<ManagedExcludeBlock>>,
) -> (BatchRequest, Vec<u8>) {
    let mut raw = Vec::new();
    let items = writes
        .iter()
        .enumerate()
        .map(|(index, (path, bytes, expected))| {
            let offset = raw.len() as u64;
            raw.extend_from_slice(bytes);
            BatchItem {
                index,
                operation_id: uuid(),
                path: path.clone(),
                offset,
                length: bytes.len() as u64,
                hash: hash(bytes),
                mode: 0o644,
                expected: expected.clone(),
                dependencies: vec![],
            }
        })
        .collect();
    let digest = hash(&raw);
    let manifest = BatchManifest {
        request_id: uuid(),
        generation: 1,
        codec: Codec::Identity,
        raw_length: raw.len() as u64,
        encoded_length: raw.len() as u64,
        raw_digest: digest,
        encoded_digest: digest,
        items,
        removes,
        chmods,
        ensure_dirs,
        exclude_block,
    };
    manifest.validate().unwrap();
    (
        BatchRequest {
            batch_id: uuid(),
            manifest_digest: manifest.digest().unwrap(),
            operation: BatchOperation::Begin { manifest },
            ..request.clone()
        },
        raw,
    )
}
pub(crate) fn upload_any(root: &Path, request: &BatchRequest, encoded: &[u8]) {
    if encoded.is_empty() {
        call(root, request, request.operation.clone()).unwrap();
        assert_eq!(
            call(root, request, BatchOperation::Seal).unwrap().phase,
            TransferPhase::Verified
        );
    } else {
        upload(root, request, encoded);
    }
}
pub(crate) fn exclude_text(repo: &str) -> String {
    std::fs::read_to_string(Path::new(repo).join(".git/info/exclude")).unwrap_or_default()
}
fn applied_effect(outcome: &MemberOutcome) -> bool {
    matches!(
        outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Applied { .. }
        }
    )
}

/// 10 writes, 3 removes, 2 chmods, a directory and an exclude
/// block apply in ONE Apply. A pre-hash mismatch on one write fails only that
/// entry, which is reported; everything else lands.
#[test]
fn one_apply_writes_removes_chmods_and_excludes_and_isolates_a_pre_hash_mismatch() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    // The fixture's clone sits next to its worktree.
    let repo = Path::new(&cwd).parent().unwrap().join("clone");
    let cwd = Path::new(&cwd);
    std::fs::create_dir_all(cwd.join("ctx")).unwrap();
    for name in ["old0", "old1", "old2"] {
        std::fs::write(cwd.join("ctx").join(name), name.as_bytes()).unwrap();
    }
    std::fs::write(cwd.join("ctx/run.sh"), b"#!/bin/sh\n").unwrap();
    std::fs::write(cwd.join("ctx/tool.sh"), b"#!/bin/sh\necho\n").unwrap();
    // The one entry whose pre-hash no longer holds: the user edited it.
    std::fs::write(cwd.join("ctx/w3.md"), b"user edit since the diff").unwrap();
    let writes: Vec<(String, Vec<u8>, Expected)> = (0..10)
        .map(|i| {
            let expected = if i == 3 {
                Expected::Hash {
                    hash: hash(b"what the diff saw"),
                }
            } else {
                Expected::Absent
            };
            (
                format!("ctx/w{i}.md"),
                format!("write {i}").into_bytes(),
                expected,
            )
        })
        .collect();
    let removes = ["old0", "old1", "old2"]
        .iter()
        .map(|name| RemoveItem {
            operation_id: uuid(),
            path: format!("ctx/{name}"),
            expected_pre_hash: hash(name.as_bytes()),
        })
        .collect();
    let chmods = vec![
        ChmodItem {
            operation_id: uuid(),
            path: "ctx/run.sh".into(),
            mode: 0o755,
            expected_pre_hash: hash(b"#!/bin/sh\n"),
        },
        ChmodItem {
            operation_id: uuid(),
            path: "ctx/tool.sh".into(),
            mode: 0o755,
            expected_pre_hash: hash(b"#!/bin/sh\necho\n"),
        },
    ];
    let ensure_dirs = vec![DirItem {
        operation_id: uuid(),
        path: "ctx/empty/dir".into(),
    }];
    let expected: Vec<String> = ["old0", "old1", "old2"]
        .iter()
        .map(|name| format!("ctx/{name}"))
        .collect();
    let mut wanted: Vec<String> = (0..10).map(|i| format!("ctx/w{i}.md")).collect();
    wanted.push("ctx/odd [x]*.md".into());
    let block = ManagedExcludeBlock {
        operation_id: uuid(),
        workspace_id: "ws-alpha".into(),
        expected: expected.clone(),
        paths: wanted,
        repair: false,
    };
    // A previous launch left this workspace's block listing the old files,
    // and another workspace of the same clone has its own block.
    let info = repo.join(".git/info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::write(
        info.join("exclude"),
        "# user line\n\
         # >>> codesense managed workspace ws-alpha\n/ctx/old0\n/ctx/old1\n/ctx/old2\n# <<< codesense managed workspace ws-alpha\n\
         # >>> codesense managed workspace ws-beta\n/beta/file\n# <<< codesense managed workspace ws-beta\n",
    )
    .unwrap();
    let (request, raw) = mixed_batch(
        &request,
        &writes,
        removes,
        chmods,
        ensure_dirs,
        Some(Box::new(block)),
    );
    upload_any(root.path(), &request, &raw);
    let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
    // Exactly one failure: the stale-pre-hash write, reported as a conflict.
    let failed: Vec<_> = applied
        .items
        .iter()
        .filter(|item| !successful(&item.outcome))
        .collect();
    assert_eq!(failed.len(), 1, "{:?}", applied.items);
    assert_eq!(failed[0].index, 3);
    assert!(matches!(
        failed[0].outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Conflict { .. }
        }
    ));
    assert_eq!(
        std::fs::read(cwd.join("ctx/w3.md")).unwrap(),
        b"user edit since the diff"
    );
    for i in (0..10).filter(|i| *i != 3) {
        assert_eq!(
            std::fs::read(cwd.join(format!("ctx/w{i}.md"))).unwrap(),
            format!("write {i}").as_bytes()
        );
    }
    assert_eq!(applied.operations.len(), 3 + 2 + 1 + 1);
    assert!(
        applied
            .operations
            .iter()
            .all(|operation| applied_effect(&operation.outcome)),
        "{:?}",
        applied.operations
    );
    for name in ["old0", "old1", "old2"] {
        assert!(!cwd.join("ctx").join(name).exists());
    }
    for name in ["run.sh", "tool.sh"] {
        assert_eq!(
            std::fs::metadata(cwd.join("ctx").join(name))
                .unwrap()
                .mode()
                & 0o777,
            0o755
        );
    }
    assert!(cwd.join("ctx/empty/dir").is_dir());
    let text = exclude_text(repo.to_str().unwrap());
    assert!(text.starts_with("# user line\n"), "{text}");
    assert!(
        text.contains("/beta/file"),
        "another workspace's block is untouched: {text}"
    );
    assert!(text.contains("/ctx/w0.md"));
    assert!(text.contains("/ctx/odd \\[x\\]\\*.md"), "escaped: {text}");
    assert!(!text.contains("/ctx/old0"), "removed files leave the block");
    assert!(
        !text.contains("/ctx/w3.md"),
        "a failed new write must not hide the user's file: {text}"
    );
    // The block the batch wrote is the one `git` honours, escapes included:
    // the literal name is ignored, a name its glob would match is not.
    std::fs::write(cwd.join("ctx/odd [x]*.md"), b"x").unwrap();
    std::fs::write(cwd.join("ctx/odd x-and-more.md"), b"x").unwrap();
    let ignored = std::process::Command::new("git")
        .args([
            "check-ignore",
            "ctx/w0.md",
            "ctx/odd [x]*.md",
            "ctx/odd x-and-more.md",
            "ctx/w3.md",
        ])
        .current_dir(cwd)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&ignored.stdout),
        "ctx/w0.md\nctx/odd [x]*.md\n"
    );
    assert_eq!(
        call(
            root.path(),
            &request,
            BatchOperation::Finish {
                disposition: FinishDisposition::Settled
            }
        )
        .unwrap()
        .phase,
        TransferPhase::Finished
    );
}

/// A retried Apply of the same batch returns the stored outcomes and
/// re-executes nothing; a NEW attempt (new batch and operation IDs) is never
/// answered from the old receipts.
#[test]
fn apply_is_idempotent_by_operation_id_and_a_new_attempt_runs_fresh() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let cwd = Path::new(&cwd);
    std::fs::write(cwd.join("stale.md"), b"stale").unwrap();
    let writes = vec![("fresh.md".to_string(), b"fresh".to_vec(), Expected::Absent)];
    let remove = RemoveItem {
        operation_id: uuid(),
        path: "stale.md".into(),
        expected_pre_hash: hash(b"stale"),
    };
    let (first, raw) = mixed_batch(
        &request,
        &writes,
        vec![remove.clone()],
        vec![],
        vec![],
        None,
    );
    upload_any(root.path(), &first, &raw);
    let applied = call(root.path(), &first, BatchOperation::Apply).unwrap();
    assert!(applied.items.iter().all(|item| successful(&item.outcome)));
    assert!(applied_effect(&applied.operations[0].outcome));
    // Between the lost acknowledgement and the retry, the user recreates the
    // removed file and edits the written one.
    std::fs::write(cwd.join("stale.md"), b"stale").unwrap();
    std::fs::write(cwd.join("fresh.md"), b"user edit").unwrap();
    let replay = call(root.path(), &first, BatchOperation::Apply).unwrap();
    assert_eq!(replay.items, applied.items, "stored outcomes, not re-run");
    assert_eq!(replay.operations, applied.operations);
    assert!(cwd.join("stale.md").exists(), "the replay removed nothing");
    assert_eq!(std::fs::read(cwd.join("fresh.md")).unwrap(), b"user edit");
    assert_eq!(
        call(root.path(), &first, BatchOperation::Status)
            .unwrap()
            .operations,
        applied.operations
    );
    call(
        root.path(),
        &first,
        BatchOperation::Finish {
            disposition: FinishDisposition::Settled,
        },
    )
    .unwrap();
    // A new attempt for the same paths gets new IDs and is judged afresh:
    // the edited file is a conflict now, the recreated one is removed again.
    let writes = vec![(
        "fresh.md".to_string(),
        b"fresh".to_vec(),
        Expected::Hash {
            hash: hash(b"fresh"),
        },
    )];
    let again = RemoveItem {
        operation_id: uuid(),
        ..remove
    };
    let (second, raw) = mixed_batch(&request, &writes, vec![again], vec![], vec![], None);
    upload_any(root.path(), &second, &raw);
    let applied = call(root.path(), &second, BatchOperation::Apply).unwrap();
    assert!(matches!(
        applied.items[0].outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Conflict { .. }
        }
    ));
    assert!(applied_effect(&applied.operations[0].outcome));
    assert!(!cwd.join("stale.md").exists());
}

/// The exclude block is a compare-and-swap. When another writer
/// changed this workspace's block since the diff, nothing is written.
#[test]
fn a_stale_exclude_block_writes_nothing() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let cwd = Path::new(&cwd);
    let repo = cwd.parent().unwrap().join("clone");
    std::fs::create_dir_all(repo.join(".git/info")).unwrap();
    std::fs::write(
        repo.join(".git/info/exclude"),
        "# >>> codesense managed workspace ws-alpha\n/other/launch\n# <<< codesense managed workspace ws-alpha\n",
    )
    .unwrap();
    std::fs::write(cwd.join("gone.md"), b"gone").unwrap();
    let writes = vec![("new.md".to_string(), b"new".to_vec(), Expected::Absent)];
    let block = ManagedExcludeBlock {
        operation_id: uuid(),
        workspace_id: "ws-alpha".into(),
        expected: vec![],
        paths: vec!["new.md".into()],
        repair: false,
    };
    let remove = RemoveItem {
        operation_id: uuid(),
        path: "gone.md".into(),
        expected_pre_hash: hash(b"gone"),
    };
    let (request, raw) = mixed_batch(
        &request,
        &writes,
        vec![remove],
        vec![],
        vec![],
        Some(Box::new(block)),
    );
    upload_any(root.path(), &request, &raw);
    let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert!(matches!(
        applied.items[0].outcome,
        MemberOutcome::Skipped { .. }
    ));
    assert!(!cwd.join("new.md").exists());
    assert!(cwd.join("gone.md").exists());
    assert!(applied.operations.iter().any(|operation| matches!(
        operation.outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Conflict { .. }
        }
    )));
    assert!(exclude_text(repo.to_str().unwrap()).contains("/other/launch"));
    // Nothing is left unresolved: the batch settles.
    assert_eq!(
        call(
            root.path(),
            &request,
            BatchOperation::Finish {
                disposition: FinishDisposition::Settled
            }
        )
        .unwrap()
        .phase,
        TransferPhase::Finished
    );
}

/// Staging carries a TTL. An upload abandoned mid-way (no
/// effect dispatched) and older than the TTL no longer blocks a new batch; a
/// fresh one still does.
#[test]
fn a_stale_unapplied_staging_expires_and_a_fresh_one_still_blocks() {
    let (root, _cwd, request, encoded) = fixture_batch(Codec::Identity);
    call(root.path(), &request, request.operation.clone()).unwrap();
    call(
        root.path(),
        &request,
        BatchOperation::Chunk {
            offset: 0,
            bytes: encoded[..4].to_vec(),
        },
    )
    .unwrap();
    let (next, raw) = large_batch(&request, 1, 16);
    let busy = call(root.path(), &next, next.operation.clone()).unwrap_err();
    assert!(busy.contains("busy"), "{busy}");
    // Age the abandoned upload past the TTL.
    let records: Vec<_> = walk_files(root.path())
        .into_iter()
        .filter(|path| {
            path.file_name().is_some_and(|name| name == "record.json")
                && path.to_string_lossy().contains(&request.batch_id)
        })
        .collect();
    assert_eq!(records.len(), 1);
    let old = std::time::SystemTime::now() - STAGING_TTL - Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&records[0])
        .unwrap()
        .set_modified(old)
        .unwrap();
    upload(root.path(), &next, &raw);
    assert!(call(root.path(), &next, BatchOperation::Apply)
        .unwrap()
        .items
        .iter()
        .all(|item| successful(&item.outcome)));
    // The expired batch is closed for good.
    assert_eq!(
        call(root.path(), &request, BatchOperation::Status)
            .unwrap()
            .phase,
        TransferPhase::Abandoned
    );
}
fn walk_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

pub(crate) fn finish_settled(
    root: &Path,
    request: &BatchRequest,
) -> Result<TransferStatus, String> {
    call(
        root,
        request,
        BatchOperation::Finish {
            disposition: FinishDisposition::Settled,
        },
    )
}

/// A batch cut mid-Apply (its desktop died or timed out) never wedges the
/// workspace. A new batch is told which one to recover; its own Begin+Apply
/// resumes it; once settled it is finished automatically; after the TTL an
/// unsettled one is purged, and Status still reports what it did.
#[test]
fn a_batch_cut_mid_apply_is_resumed_settled_or_purged() {
    let (root, cwd, request, encoded) = fixture_batch(Codec::Identity);
    upload(root.path(), &request, &encoded);
    let first = Path::new(&cwd).join("nested/context/0.md");
    let second = Path::new(&cwd).join("nested/context/1.md");
    assert!(handle(
        root.path(),
        41,
        BatchRequest {
            operation: BatchOperation::Apply,
            ..request.clone()
        },
        || !first.exists()
    )
    .is_err());
    // A new attempt is refused, naming the batch to recover.
    let (next, raw) = large_batch(&request, 1, 16);
    let busy = call(root.path(), &next, next.operation.clone()).unwrap_err();
    assert!(busy.contains(&request.batch_id), "{busy}");
    // The cut batch's own Begin then Apply resumes it.
    call(root.path(), &request, request.operation.clone()).unwrap();
    let resumed = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert!(resumed.items.iter().all(|item| successful(&item.outcome)));
    assert!(second.exists());
    // Never finished: the next batch's Begin finishes it, since it settled.
    upload(root.path(), &next, &raw);
    assert_eq!(
        call(root.path(), &request, BatchOperation::Status)
            .unwrap()
            .phase,
        TransferPhase::Finished
    );
    assert!(call(root.path(), &next, BatchOperation::Apply)
        .unwrap()
        .items
        .iter()
        .all(|item| successful(&item.outcome)));
    finish_settled(root.path(), &next).unwrap();
}

#[test]
fn an_unsettled_cut_batch_is_purged_after_the_ttl_with_its_outcomes_kept() {
    let (root, cwd, request, encoded) = fixture_batch(Codec::Identity);
    upload(root.path(), &request, &encoded);
    let first = Path::new(&cwd).join("nested/context/0.md");
    assert!(handle(
        root.path(),
        41,
        BatchRequest {
            operation: BatchOperation::Apply,
            ..request.clone()
        },
        || !first.exists()
    )
    .is_err());
    for record in walk_files(root.path()).into_iter().filter(|path| {
        path.file_name().is_some_and(|name| name == "record.json")
            && path.to_string_lossy().contains(&request.batch_id)
    }) {
        std::fs::File::options()
            .write(true)
            .open(&record)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - STAGING_TTL - Duration::from_secs(60))
            .unwrap();
    }
    let (next, raw) = large_batch(&request, 1, 16);
    upload(root.path(), &next, &raw);
    let purged = call(root.path(), &request, BatchOperation::Status).unwrap();
    assert_eq!(purged.phase, TransferPhase::Abandoned);
    assert!(
        successful(&purged.items[0].outcome),
        "what it did is still reported"
    );
    assert!(!Path::new(&cwd).join("nested/context/1.md").exists());
}

/// A settled batch keeps its outcomes, not its bytes; identity checks
/// and Status still work on the compacted receipts.
#[test]
fn settlement_drops_stored_bytes_and_keeps_outcomes() {
    let (root, _cwd, request, encoded) = fixture_batch(Codec::Identity);
    let (request, raw) = large_batch(&request, 2, 256 * 1024);
    upload(root.path(), &request, &raw);
    let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
    let _ = encoded;
    let ledger_bytes = |root: &Path| -> u64 {
        walk_files(root)
            .iter()
            .filter(|path| path.to_string_lossy().contains("remote-file-operations"))
            .map(|path| std::fs::metadata(path).unwrap().len())
            .sum()
    };
    let before = ledger_bytes(root.path());
    assert!(before > 512 * 1024, "receipts hold the bytes until settled");
    finish_settled(root.path(), &request).unwrap();
    let after = ledger_bytes(root.path());
    assert!(after < 64 * 1024, "{after}");
    let status = call(root.path(), &request, BatchOperation::Status).unwrap();
    assert_eq!(status.items, applied.items);
    let BatchOperation::Begin { manifest } = &request.operation else {
        unreachable!()
    };
    let reconciled = files::handle(
        root.path(),
        41,
        FileRequest {
            target: request.target.clone(),
            operation: FileOperation::Reconcile {
                operation_id: manifest.items[0].operation_id.clone(),
            },
        },
        || true,
    )
    .unwrap();
    assert!(matches!(
        reconciled,
        FileReply::Effect {
            outcome: EffectOutcome::Applied { .. }
        }
    ));
}

/// A hand-edited workspace block (a foreign line) is a conflict with a
/// repair path, never an error; the repair replaces only this block.
#[test]
fn a_foreign_line_in_the_block_conflicts_and_an_explicit_repair_replaces_it() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let repo = Path::new(&cwd).parent().unwrap().join("clone");
    std::fs::create_dir_all(repo.join(".git/info")).unwrap();
    std::fs::write(
        repo.join(".git/info/exclude"),
        "# >>> codesense managed workspace ws-alpha\n/a.md\n*.log\n# <<< codesense managed workspace ws-alpha\n\
         # >>> codesense managed workspace ws-beta\n/beta\n# <<< codesense managed workspace ws-beta\n",
    )
    .unwrap();
    let block = |repair| ManagedExcludeBlock {
        operation_id: uuid(),
        workspace_id: "ws-alpha".into(),
        expected: vec!["a.md".into()],
        paths: vec!["b.md".into()],
        repair,
    };
    let writes = vec![("b.md".to_string(), b"b".to_vec(), Expected::Absent)];
    let (plain, raw) = mixed_batch(
        &request,
        &writes,
        vec![],
        vec![],
        vec![],
        Some(Box::new(block(false))),
    );
    upload_any(root.path(), &plain, &raw);
    let refused = call(root.path(), &plain, BatchOperation::Apply).unwrap();
    assert!(matches!(
        refused.items[0].outcome,
        MemberOutcome::Skipped { .. }
    ));
    finish_settled(root.path(), &plain).unwrap();
    let (repair, raw) = mixed_batch(
        &request,
        &writes,
        vec![],
        vec![],
        vec![],
        Some(Box::new(block(true))),
    );
    upload_any(root.path(), &repair, &raw);
    let repaired = call(root.path(), &repair, BatchOperation::Apply).unwrap();
    assert!(successful(&repaired.items[0].outcome), "{repaired:?}");
    let text = exclude_text(repo.to_str().unwrap());
    assert!(!text.contains("*.log") && !text.contains("/a.md"), "{text}");
    assert!(text.contains("/b.md") && text.contains("/beta"), "{text}");
}

/// Low: on a case-insensitive filesystem a removal that shares a write's
/// case-folded name runs first, so the write does not meet the old file.
#[test]
fn a_case_folded_removal_runs_before_the_write() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    std::fs::write(Path::new(&cwd).join("Readme.md"), b"old").unwrap();
    let writes = vec![("README.md".to_string(), b"new".to_vec(), Expected::Absent)];
    let remove = RemoveItem {
        operation_id: uuid(),
        path: "Readme.md".into(),
        expected_pre_hash: hash(b"old"),
    };
    let (batch, raw) = mixed_batch(&request, &writes, vec![remove], vec![], vec![], None);
    upload_any(root.path(), &batch, &raw);
    let applied = call(root.path(), &batch, BatchOperation::Apply).unwrap();
    assert!(successful(&applied.items[0].outcome), "{applied:?}");
    assert!(successful(&applied.operations[0].outcome));
    assert_eq!(
        std::fs::read(Path::new(&cwd).join("README.md")).unwrap(),
        b"new"
    );
}

/// Must-fix 1: a file stored under a decomposed (NFD) name is found by NFC
/// equivalence: a remove by its NFC path removes it (never a false
/// `Applied(Absent)` with the file still there), and the exclude block names
/// it in its on-disk spelling, so Git ignores it.
#[test]
fn an_nfd_named_file_is_removed_and_excluded_by_its_nfc_path() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let cwd = Path::new(&cwd);
    std::fs::create_dir_all(cwd.join("ctx")).unwrap();
    let stored_old = "ctx/cafe\u{301}-old.md";
    let stored_kept = "ctx/cafe\u{301}-kept.md";
    std::fs::write(cwd.join(stored_old), b"old").unwrap();
    std::fs::write(cwd.join(stored_kept), b"kept").unwrap();
    let remove = RemoveItem {
        operation_id: uuid(),
        path: "ctx/caf\u{e9}-old.md".into(),
        expected_pre_hash: hash(b"old"),
    };
    let block = ManagedExcludeBlock {
        operation_id: uuid(),
        workspace_id: "ws-nfd".into(),
        expected: vec![],
        paths: vec!["ctx/caf\u{e9}-kept.md".into()],
        repair: false,
    };
    let writes = vec![(
        "ctx/plain.md".to_string(),
        b"plain".to_vec(),
        Expected::Absent,
    )];
    let (batch, raw) = mixed_batch(
        &request,
        &writes,
        vec![remove],
        vec![],
        vec![],
        Some(Box::new(block)),
    );
    upload_any(root.path(), &batch, &raw);
    let applied = call(root.path(), &batch, BatchOperation::Apply).unwrap();
    let removed = &applied
        .operations
        .iter()
        .find(|op| op.kind == OperationKind::Remove)
        .unwrap()
        .outcome;
    assert!(successful(removed), "{removed:?}");
    assert!(
        !cwd.join(stored_old).exists(),
        "an Applied removal never leaves the file in place"
    );
    let ignored = std::process::Command::new("git")
        .args([
            "-c",
            "core.quotepath=false",
            "check-ignore",
            "--no-index",
            stored_kept,
        ])
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        ignored.status.success(),
        "the NFD file is ignored: {ignored:?}"
    );
}

#[test]
fn nfc_equivalence_is_byte_exact_and_refuses_ambiguity() {
    let names = vec!["cafe\u{301}.md".to_string(), "other.md".to_string()];
    assert_eq!(
        files::nfc_equivalent(&names, "caf\u{e9}.md")
            .unwrap()
            .as_deref(),
        Some("cafe\u{301}.md")
    );
    assert_eq!(files::nfc_equivalent(&names, "absent.md").unwrap(), None);
    let ambiguous = vec!["cafe\u{301}.md".to_string(), "caf\u{e9}.md".to_string()];
    assert!(files::nfc_equivalent(&ambiguous, "caf\u{e9}.md").is_err());
}

/// Must-fix 4: before a cut batch is purged, its exclude block takes its
/// final shape from the stored outcomes: the path it never wrote leaves the
/// block, and Status reports the block as left.
#[test]
fn purging_a_cut_batch_first_finalizes_its_exclude_block() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let cwd = Path::new(&cwd);
    let repo = cwd.parent().unwrap().join("clone");
    let writes = vec![
        ("ctx/a.md".to_string(), b"a".to_vec(), Expected::Absent),
        ("ctx/b.md".to_string(), b"b".to_vec(), Expected::Absent),
    ];
    let block = ManagedExcludeBlock {
        operation_id: uuid(),
        workspace_id: "ws-cut".into(),
        expected: vec![],
        paths: vec!["ctx/a.md".into(), "ctx/b.md".into()],
        repair: false,
    };
    let (batch, raw) = mixed_batch(
        &request,
        &writes,
        vec![],
        vec![],
        vec![],
        Some(Box::new(block)),
    );
    upload_any(root.path(), &batch, &raw);
    let first = cwd.join("ctx/a.md");
    assert!(handle(
        root.path(),
        41,
        BatchRequest {
            operation: BatchOperation::Apply,
            ..batch.clone()
        },
        || !first.exists()
    )
    .is_err());
    assert!(
        exclude_text(repo.to_str().unwrap()).contains("/ctx/b.md"),
        "added before writes"
    );
    for record in walk_files(root.path()).into_iter().filter(|path| {
        path.file_name().is_some_and(|name| name == "record.json")
            && path.to_string_lossy().contains(&batch.batch_id)
    }) {
        std::fs::File::options()
            .write(true)
            .open(&record)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - STAGING_TTL - Duration::from_secs(60))
            .unwrap();
    }
    let (next, raw) = large_batch(&request, 1, 16);
    upload(root.path(), &next, &raw);
    let text = exclude_text(repo.to_str().unwrap());
    assert!(text.contains("/ctx/a.md"), "{text}");
    assert!(
        !text.contains("/ctx/b.md"),
        "the never-written path left the block: {text}"
    );
    let purged = call(root.path(), &batch, BatchOperation::Status).unwrap();
    assert_eq!(purged.phase, TransferPhase::Abandoned);
    assert_eq!(purged.exclude_paths, Some(vec!["ctx/a.md".to_string()]));
}
