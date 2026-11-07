// SPDX-License-Identifier: MIT
//! The batch Apply's fsync budget, its journal, and its crash
//! safety at every phase boundary.
use super::tests::{call, finish_settled, fixture_batch, hash, mixed_batch, upload_any, uuid};
use super::*;
use codesense_pty_core::remote_files::Expected;

fn staging_of(root: &Path, request: &BatchRequest) -> File {
    let ledger = files::batch_ledger(
        root,
        41,
        &request.target,
        &files::locate(root, 41, &request.target, &|| true).unwrap(),
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
    directory(&workspace, &request.batch_id, false).unwrap()
}
fn journal_names(root: &Path, request: &BatchRequest) -> Vec<String> {
    let mut names: Vec<String> = files::children(&staging_of(root, request))
        .unwrap()
        .into_iter()
        .filter(|name| name.contains("journal-"))
        .collect();
    names.sort();
    names
}
fn ledger_files(root: &Path) -> usize {
    std::fs::read_dir(root.join("remote-file-operations"))
        .unwrap()
        .filter(|entry| entry.as_ref().unwrap().file_type().unwrap().is_file())
        .count()
}
fn manifest_of(request: &BatchRequest) -> &BatchManifest {
    let BatchOperation::Begin { manifest } = &request.operation else {
        unreachable!()
    };
    manifest
}
/// (inode, mtime ns) of a worktree file.
fn identity(path: &Path) -> (u64, i64) {
    let metadata = std::fs::metadata(path).unwrap();
    (
        metadata.ino(),
        metadata.mtime_nsec() + metadata.mtime() * 1_000_000_000,
    )
}
fn stray_temporaries(cwd: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![cwd.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            } else if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".codesense-file-")
            {
                found.push(entry.path());
            }
        }
    }
    found
}
/// Run `operation`, dying (a panic, as a killed process would) at `point`.
fn die_at(root: &Path, request: &BatchRequest, operation: BatchOperation, point: &'static str) {
    arm_test_hold(point, || panic!("injected crash"));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = call(root, request, operation);
    }));
    assert!(outcome.is_err(), "the crash at {point} fired");
}

/// 25 writes in new nested directories and the worktree root, 2 removals
/// and 2 mode changes under an existing directory, and one new directory.
fn budget_batch() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    BatchRequest,
    Vec<u8>,
    usize,
) {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let cwd = std::path::PathBuf::from(cwd);
    std::fs::create_dir_all(cwd.join("old")).unwrap();
    for name in ["r0", "r1", "c0", "c1"] {
        std::fs::write(cwd.join("old").join(name), name.as_bytes()).unwrap();
    }
    let mut writes = Vec::new();
    for index in 0..10 {
        writes.push((
            format!("a/x/f{index}.md"),
            format!("a{index}").into_bytes(),
            Expected::Absent,
        ));
        writes.push((
            format!("b/f{index}.md"),
            format!("b{index}").into_bytes(),
            Expected::Absent,
        ));
    }
    for index in 0..5 {
        writes.push((
            format!("top{index}.md"),
            format!("t{index}").into_bytes(),
            Expected::Absent,
        ));
    }
    let removes = ["r0", "r1"]
        .iter()
        .map(|name| RemoveItem {
            operation_id: uuid(),
            path: format!("old/{name}"),
            expected_pre_hash: hash(name.as_bytes()),
        })
        .collect();
    let chmods = ["c0", "c1"]
        .iter()
        .map(|name| ChmodItem {
            operation_id: uuid(),
            path: format!("old/{name}"),
            mode: 0o600,
            expected_pre_hash: hash(name.as_bytes()),
        })
        .collect();
    let dirs = vec![DirItem {
        operation_id: uuid(),
        path: "e/new".into(),
    }];
    let writes_count = writes.len();
    let (request, raw) = mixed_batch(&request, &writes, removes, chmods, dirs, None);
    (root, cwd, request, raw, writes_count)
}

/// An Apply of W writes and C mode changes touching D directories makes
/// at most W + C + D + 4 fsync-class calls, exactly 4 of them flushes (the
/// dispatch checkpoint and the journal commit, 2 each), and writes no
/// per-member receipt. A re-sent Apply, Status and Finish cost what they must.
#[test]
fn an_apply_makes_at_most_one_sync_per_entry_and_directory() {
    let (root, cwd, request, raw, writes) = budget_batch();
    upload_any(root.path(), &request, &raw);
    let receipts_before = ledger_files(root.path());
    let meter = durable::Meter::start();
    let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
    let syncs = meter.read();
    assert!(applied.items.iter().all(|item| successful(&item.outcome)));
    assert!(applied
        .operations
        .iter()
        .all(|operation| successful(&operation.outcome)));
    // Touched: the worktree (a, b, e and the top files), a (x), a/x, b, e
    // (new), old (the removals).
    let (chmods, directories) = (2u64, 6u64);
    assert_eq!(syncs.flushes, 4, "{syncs:?}");
    assert!(
        syncs.orders <= writes as u64 + chmods + directories,
        "{syncs:?}"
    );
    assert!(syncs.total() <= writes as u64 + chmods + directories + 4);
    assert_eq!(
        ledger_files(root.path()),
        receipts_before,
        "no per-member receipts"
    );
    assert_eq!(journal_names(root.path(), &request).len(), 1);
    assert_eq!(std::fs::read(cwd.join("a/x/f3.md")).unwrap(), b"a3");
    assert!(cwd.join("e/new").is_dir());
    assert!(!cwd.join("old/r0").exists());
    assert_eq!(
        std::fs::metadata(cwd.join("old/c1")).unwrap().mode() & 0o777,
        0o600
    );
    // A re-sent Apply settles nothing new: no effect, no segment, no sync.
    let meter = durable::Meter::start();
    let again = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert_eq!(meter.read().total(), 0);
    assert_eq!(again.items, applied.items);
    assert_eq!(journal_names(root.path(), &request).len(), 1);
    // Status never syncs a directory that already exists.
    let meter = durable::Meter::start();
    call(root.path(), &request, BatchOperation::Status).unwrap();
    assert_eq!(meter.read().total(), 0);
    // Finish: its tombstone (2), the payload cleanup (1) and, at most once
    // an hour, the receipt ledger's prune (1).
    let meter = durable::Meter::start();
    finish_settled(root.path(), &request).unwrap();
    assert!(meter.read().total() <= 4, "{:?}", meter.read());
}

/// A chunk is 3 fsyncs (its payload and its record checkpoint),
/// never the directory syncs every call used to make.
#[test]
fn a_chunk_and_a_status_sync_only_what_they_change() {
    let (root, _cwd, request, encoded) = fixture_batch(Codec::Identity);
    call(root.path(), &request, request.operation.clone()).unwrap();
    let meter = durable::Meter::start();
    call(
        root.path(),
        &request,
        BatchOperation::Chunk {
            offset: 0,
            bytes: encoded.clone(),
        },
    )
    .unwrap();
    assert!(meter.read().total() <= 3, "{:?}", meter.read());
    let meter = durable::Meter::start();
    call(root.path(), &request, BatchOperation::Status).unwrap();
    assert_eq!(meter.read().total(), 0);
}

/// A crash at each phase boundary of an Apply, then the same Apply
/// again: every target ends at its bytes, what already landed is not
/// rewritten, no temporary is left, and Status, the journal and a per-op
/// Reconcile agree on the applied set.
#[test]
fn a_crash_at_each_phase_boundary_resumes_without_rewriting() {
    for point in [
        "member-write",
        "between-members",
        "before-seal",
        "after-seal",
        "journal-staged",
    ] {
        let (root, cwd, request, raw, _) = budget_batch();
        upload_any(root.path(), &request, &raw);
        die_at(root.path(), &request, BatchOperation::Apply, point);
        if point == "journal-staged" {
            let names = journal_names(root.path(), &request);
            assert!(
                names.iter().any(|name| name.ends_with(".tmp"))
                    && !names.iter().any(|name| name.ends_with(".json")),
                "the crash left a staged, uncommitted segment: {names:?}"
            );
        }
        let manifest = manifest_of(&request);
        let landed: Vec<(std::path::PathBuf, (u64, i64))> = manifest
            .items
            .iter()
            .map(|item| cwd.join(&item.path))
            .filter(|path| path.exists())
            .map(|path| {
                let id = identity(&path);
                (path, id)
            })
            .collect();
        let resumed = call(root.path(), &request, BatchOperation::Apply).unwrap();
        assert!(
            resumed.items.iter().all(|item| successful(&item.outcome)),
            "{point}: {:?}",
            resumed.items
        );
        assert!(resumed
            .operations
            .iter()
            .all(|operation| successful(&operation.outcome)));
        for (path, before) in &landed {
            assert_eq!(identity(path), *before, "{point}: {path:?} was rewritten");
        }
        for item in &manifest.items {
            let bytes = std::fs::read(cwd.join(&item.path)).unwrap();
            assert_eq!(hash(&bytes), item.hash, "{point}: {}", item.path);
        }
        assert!(stray_temporaries(&cwd).is_empty(), "{point}");
        assert!(
            journal_names(root.path(), &request)
                .iter()
                .all(|name| name.ends_with(".json")),
            "{point}: a staged segment survived"
        );
        let status = call(root.path(), &request, BatchOperation::Status).unwrap();
        assert_eq!(status.items, resumed.items, "{point}");
        for item in &manifest.items {
            let reply = files::handle(
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
            assert!(
                matches!(
                    reply,
                    FileReply::Effect {
                        outcome: EffectOutcome::Applied { .. }
                    }
                ),
                "{point}: {reply:?}"
            );
        }
        assert_eq!(
            finish_settled(root.path(), &request).unwrap().phase,
            TransferPhase::Finished
        );
    }
}

/// A member that landed before the crash and was then edited by the
/// user is a conflict on resume; the user's bytes stay.
#[test]
fn a_user_edit_between_the_crash_and_the_resume_is_kept() {
    let (root, cwd, request, raw, _) = budget_batch();
    upload_any(root.path(), &request, &raw);
    die_at(root.path(), &request, BatchOperation::Apply, "after-seal");
    let first = &manifest_of(&request).items[0];
    std::fs::write(cwd.join(&first.path), b"the user's edit").unwrap();
    let resumed = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert!(matches!(
        resumed.items[0].outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Conflict { .. }
        }
    ));
    assert!(resumed.items[1..]
        .iter()
        .all(|item| successful(&item.outcome)));
    assert_eq!(
        std::fs::read(cwd.join(&first.path)).unwrap(),
        b"the user's edit"
    );
}

/// After a crash before the journal commit, Status, Apply or Finish
/// recovers the landed members as applied, journals that proof, and a later
/// legitimate edit never turns it back.
#[test]
fn recovered_batch_proof_stays_applied_after_later_edits() {
    for operation in [
        BatchOperation::Status,
        BatchOperation::Apply,
        BatchOperation::Finish {
            disposition: FinishDisposition::Settled,
        },
    ] {
        let (root, cwd, request, encoded) = fixture_batch(Codec::Identity);
        upload_any(root.path(), &request, &encoded);
        die_at(root.path(), &request, BatchOperation::Apply, "after-seal");
        assert!(journal_names(root.path(), &request).is_empty());
        let manifest = manifest_of(&request);
        let recovered = call(root.path(), &request, operation).unwrap();
        assert!(successful(&recovered.items[0].outcome));
        assert_eq!(journal_names(root.path(), &request).len(), 1);
        let file = Path::new(&cwd).join(&manifest.items[0].path);
        std::fs::write(&file, b"legitimate later edit").unwrap();
        for _ in 0..2 {
            assert!(successful(
                &call(root.path(), &request, BatchOperation::Status)
                    .unwrap()
                    .items[0]
                    .outcome
            ));
        }
        assert_eq!(std::fs::read(file).unwrap(), b"legitimate later edit");
    }
}

/// A power loss after the renames but before the journal (modelled: the
/// target is back to its old state and no segment exists): the write runs
/// again.
#[test]
fn a_lost_rename_and_journal_are_redone() {
    let (root, cwd, request, raw, _) = budget_batch();
    upload_any(root.path(), &request, &raw);
    call(root.path(), &request, BatchOperation::Apply).unwrap();
    let staging = staging_of(root.path(), &request);
    for name in journal_names(root.path(), &request) {
        let component = self::name(&name).unwrap();
        // SAFETY: a test-owned file under the held staging directory.
        unsafe { libc::unlinkat(staging.as_raw_fd(), component.as_ptr(), 0) };
    }
    let first = &manifest_of(&request).items[0];
    std::fs::remove_file(cwd.join(&first.path)).unwrap();
    let resumed = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert!(resumed.items.iter().all(|item| successful(&item.outcome)));
    assert_eq!(
        hash(&std::fs::read(cwd.join(&first.path)).unwrap()),
        first.hash
    );
}

/// An Apply re-sent while the first is still running (its client gave
/// up at the deadline) waits for it, then reports its journaled outcomes
/// without a second write.
#[test]
fn an_apply_resent_while_the_first_runs_waits_and_writes_nothing() {
    let (root, cwd, request, raw, _) = budget_batch();
    upload_any(root.path(), &request, &raw);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let first = {
        let root = root.path().to_path_buf();
        let request = request.clone();
        std::thread::spawn(move || {
            arm_test_hold("between-members", move || {
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            call(&root, &request, BatchOperation::Apply).unwrap()
        })
    };
    held_rx.recv().unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let second = {
        let root = root.path().to_path_buf();
        let request = request.clone();
        std::thread::spawn(move || {
            let status = call(&root, &request, BatchOperation::Apply).unwrap();
            done_tx.send(()).unwrap();
            status
        })
    };
    assert!(
        done_rx
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "the re-send waits on the workspace lock"
    );
    release_tx.send(()).unwrap();
    let first = first.join().unwrap();
    let identities: Vec<_> = manifest_of(&request)
        .items
        .iter()
        .map(|item| identity(&cwd.join(&item.path)))
        .collect();
    let second = second.join().unwrap();
    assert_eq!(first.items, second.items);
    assert!(second.items.iter().all(|item| successful(&item.outcome)));
    let after: Vec<_> = manifest_of(&request)
        .items
        .iter()
        .map(|item| identity(&cwd.join(&item.path)))
        .collect();
    assert_eq!(identities, after);
    assert_eq!(journal_names(root.path(), &request).len(), 1);
}

/// A batch an older server began (per-member ledger receipts) resumes
/// through those receipts: a receipted member is reported from its receipt
/// (recovered when its outcome was never saved), never re-run, and a receipt
/// that cannot be reconciled stops Status and settlement instead of lying.
#[test]
fn a_batch_an_older_server_began_resumes_through_its_receipts() {
    let (root, cwd, request, encoded) = fixture_batch(Codec::Identity);
    upload_any(root.path(), &request, &encoded);
    let manifest = manifest_of(&request).clone();
    let item = &manifest.items[0];
    // The old server's dispatch: the Applying checkpoint, then member 0
    // through the per-operation receipt owner.
    let staging = staging_of(root.path(), &request);
    let mut current = record(&staging).unwrap();
    current.phase = TransferPhase::Applying;
    save(&staging, &current).unwrap();
    std::fs::create_dir_all(Path::new(&cwd).join(item.path.rsplit_once('/').unwrap().0)).unwrap();
    let reply = files::handle(
        root.path(),
        41,
        FileRequest {
            target: request.target.clone(),
            operation: FileOperation::Apply {
                operation_id: item.operation_id.clone(),
                path: item.path.clone(),
                expected: item.expected.clone(),
                effect: Effect::Write {
                    bytes: b"first selected file".to_vec(),
                    mode: Some(item.mode),
                },
            },
        },
        || true,
    )
    .unwrap();
    assert!(matches!(
        reply,
        FileReply::Effect {
            outcome: EffectOutcome::Applied { .. }
        }
    ));
    // Its outcome was never saved (cut after the effect).
    let ledger = files::batch_ledger(
        root.path(),
        41,
        &request.target,
        &files::locate(root.path(), 41, &request.target, &|| true).unwrap(),
        &|| true,
    )
    .unwrap();
    let key = format!(
        "{}.json",
        digest(format!("{}:{}", request.target.handle, item.operation_id).as_bytes())
    );
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&read(&ledger, &key).unwrap()).unwrap();
    receipt["outcome"] = serde_json::Value::Null;
    replace(&ledger, &key, &serde_json::to_vec(&receipt).unwrap()).unwrap();
    // The real receipt writer refuses while its own temporary is unresolved.
    let temporary = format!(
        ".workspace-write-{}",
        digest(&[key.as_bytes(), &std::process::id().to_ne_bytes()].concat())
    );
    write_new(&ledger, &temporary, b"uncommitted receipt checkpoint").unwrap();
    assert!(call(root.path(), &request, BatchOperation::Status).is_err());
    assert!(finish_settled(root.path(), &request).is_err());
    std::fs::remove_file(root.path().join("remote-file-operations").join(temporary)).unwrap();
    let landed = identity(&Path::new(&cwd).join(&item.path));
    let recovered = call(root.path(), &request, BatchOperation::Status).unwrap();
    assert!(successful(&recovered.items[0].outcome));
    let receipt: serde_json::Value = serde_json::from_slice(&read(&ledger, &key).unwrap()).unwrap();
    assert_eq!(receipt["outcome"]["kind"], "applied");
    let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert!(applied.items.iter().all(|item| successful(&item.outcome)));
    assert_eq!(identity(&Path::new(&cwd).join(&item.path)), landed);
}

/// Every fsync-class call under `remote_workspace/` goes through the
/// counted seam, so the budget tests see them all.
#[test]
fn every_fsync_goes_through_the_counted_seam() {
    fn walk(directory: &Path, sources: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, sources);
            } else {
                sources.push(path);
            }
        }
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = vec![manifest.join("src/remote_workspace.rs")];
    walk(&manifest.join("src/remote_workspace"), &mut sources);
    let mut offenders = Vec::new();
    for path in sources {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // The seam itself, and files compiled only into tests.
        let test_only =
            name.ends_with("tests.rs") || name.ends_with("_bench.rs") || name == "test_support.rs";
        if name == "durable.rs" || test_only || !name.ends_with(".rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        // A file's own trailing `mod tests` is test code.
        let production = [
            "#[cfg(test)]\nmod tests",
            "#[cfg(test)]\npub(crate) mod tests",
        ]
        .iter()
        .filter_map(|marker| text.find(marker))
        .min()
        .map_or(text.as_str(), |at| &text[..at]);
        for needle in [
            "sync_all(",
            "sync_data(",
            "fsync(",
            "F_FULLFSYNC",
            "F_BARRIERFSYNC",
            "fdatasync",
        ] {
            if production.contains(needle) {
                offenders.push(format!("{}: {needle}", path.display()));
            }
        }
    }
    assert!(offenders.is_empty(), "{offenders:?}");
}

/// A zstd payload the host refuses at Seal, with zero effects: a second
/// frame, trailing bytes, a truncated frame, a skippable frame, a frame
/// without its content size, a content size that disagrees with the
/// manifest, a dictionary frame, and a window over 16 MiB.
#[test]
fn a_hostile_or_corrupt_zstd_payload_never_seals() {
    use super::tests::zstd_encode;
    type Fault = Box<dyn Fn(&mut Vec<u8>, &mut BatchManifest)>;
    let faults: Vec<(&str, Fault)> = vec![
        (
            "second frame",
            Box::new(|encoded: &mut Vec<u8>, _: &mut BatchManifest| {
                let again = encoded.clone();
                encoded.extend_from_slice(&again);
            }),
        ),
        (
            "trailing bytes",
            Box::new(|encoded: &mut Vec<u8>, _: &mut BatchManifest| {
                encoded.extend_from_slice(b"trailing")
            }),
        ),
        (
            "truncated",
            Box::new(|encoded: &mut Vec<u8>, _: &mut BatchManifest| {
                encoded.truncate(encoded.len() - 3)
            }),
        ),
        (
            "skippable frame",
            Box::new(|encoded: &mut Vec<u8>, _: &mut BatchManifest| {
                let mut skippable = 0x184D_2A50u32.to_le_bytes().to_vec();
                skippable.extend_from_slice(&4u32.to_le_bytes());
                skippable.extend_from_slice(b"skip");
                skippable.extend_from_slice(encoded);
                *encoded = skippable;
            }),
        ),
        (
            "no content size",
            Box::new(|encoded: &mut Vec<u8>, _: &mut BatchManifest| {
                // Drop the 8-byte size and clear its flag.
                encoded[4] &= !0xC0;
                encoded.drain(6..14);
            }),
        ),
        (
            "size disagrees",
            Box::new(|_: &mut Vec<u8>, manifest: &mut BatchManifest| {
                manifest.raw_length -= 1;
                manifest.items[1].length -= 1;
            }),
        ),
        (
            "dictionary",
            Box::new(|encoded: &mut Vec<u8>, _: &mut BatchManifest| {
                encoded[4] |= 0x01;
                encoded.insert(6, 7);
            }),
        ),
        (
            "window too large",
            Box::new(|encoded: &mut Vec<u8>, _: &mut BatchManifest| {
                // Exponent 15: a 32 MiB window.
                encoded[5] = 15 << 3;
            }),
        ),
    ];
    for (name, fault) in faults {
        let (root, cwd, mut request, _) = fixture_batch(Codec::Zstd);
        let BatchOperation::Begin { manifest } = &mut request.operation else {
            unreachable!()
        };
        let mut encoded = zstd_encode(b"first selected filesecond selected file");
        fault(&mut encoded, manifest);
        manifest.encoded_length = encoded.len() as u64;
        manifest.encoded_digest = hash(&encoded);
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
        assert!(
            call(root.path(), &request, BatchOperation::Seal).is_err(),
            "{name} sealed"
        );
        assert!(call(root.path(), &request, BatchOperation::Apply).is_err());
        assert!(!Path::new(&cwd).join("nested").exists(), "{name}");
    }
}

/// A member whose effect fails stops the Apply call before the
/// exclude block's final write, so that member's path stays listed; the next
/// Apply lands it and only then finalizes the block.
#[test]
fn a_failed_member_stops_the_call_and_keeps_its_path_excluded() {
    use super::tests::exclude_text;
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let repo = Path::new(&cwd).parent().unwrap().join("clone");
    let cwd = Path::new(&cwd);
    let writes = vec![
        ("a.md".to_string(), b"a".to_vec(), Expected::Absent),
        ("b.md".to_string(), b"b".to_vec(), Expected::Absent),
    ];
    let block = ManagedExcludeBlock {
        operation_id: uuid(),
        workspace_id: "ws-alpha".into(),
        expected: vec![],
        paths: vec!["a.md".into(), "b.md".into()],
        repair: false,
    };
    let (request, raw) = mixed_batch(
        &request,
        &writes,
        vec![],
        vec![],
        vec![],
        Some(Box::new(block)),
    );
    upload_any(root.path(), &request, &raw);
    // A directory where b.md's own staged temporary goes: its removal fails.
    let blocker = cwd.join(format!(
        ".codesense-file-{}.tmp",
        manifest_of(&request).items[1].operation_id
    ));
    std::fs::create_dir(&blocker).unwrap();
    assert!(call(root.path(), &request, BatchOperation::Apply).is_err());
    assert!(cwd.join("a.md").exists());
    let excluded = exclude_text(repo.to_str().unwrap());
    assert!(
        excluded.contains("/a.md") && excluded.contains("/b.md"),
        "{excluded}"
    );
    let status = call(root.path(), &request, BatchOperation::Status).unwrap();
    assert!(successful(&status.items[0].outcome));
    assert!(status
        .operations
        .iter()
        .any(|operation| operation.kind == OperationKind::ExcludeBlock
            && is_not_started(&operation.outcome)));
    std::fs::remove_dir(&blocker).unwrap();
    let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert!(applied.items.iter().all(|item| successful(&item.outcome)));
    assert!(applied
        .operations
        .iter()
        .all(|operation| successful(&operation.outcome)));
    let excluded = exclude_text(repo.to_str().unwrap());
    assert!(
        excluded.contains("/a.md") && excluded.contains("/b.md"),
        "{excluded}"
    );
    assert_eq!(std::fs::read(cwd.join("b.md")).unwrap(), b"b");
}

/// A removal under a symlinked directory is a conflict, never
/// reported removed.
#[test]
fn a_removal_under_a_symlinked_directory_is_a_conflict() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let cwd = Path::new(&cwd);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("f.md"), b"f").unwrap();
    std::os::unix::fs::symlink(outside.path(), cwd.join("link")).unwrap();
    let remove = RemoveItem {
        operation_id: uuid(),
        path: "link/f.md".into(),
        expected_pre_hash: hash(b"f"),
    };
    let writes = vec![("x.md".to_string(), b"x".to_vec(), Expected::Absent)];
    let (request, raw) = mixed_batch(&request, &writes, vec![remove], vec![], vec![], None);
    upload_any(root.path(), &request, &raw);
    let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert!(applied.operations.iter().any(|operation| matches!(
        operation.outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Conflict { .. }
        }
    )));
    assert!(outside.path().join("f.md").exists());
}

/// After a crash before the journal commit, Status recovers the
/// extension effects that landed (removals, mode changes, directories) and
/// the resume agrees with it.
#[test]
fn status_after_a_crash_recovers_extension_effects_and_the_resume_agrees() {
    let (root, _cwd, request, raw, _) = budget_batch();
    upload_any(root.path(), &request, &raw);
    die_at(root.path(), &request, BatchOperation::Apply, "after-seal");
    let status = call(root.path(), &request, BatchOperation::Status).unwrap();
    assert!(status.items.iter().all(|item| successful(&item.outcome)));
    assert!(
        status
            .operations
            .iter()
            .all(|operation| successful(&operation.outcome)),
        "{:?}",
        status.operations
    );
    let resumed = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert_eq!(resumed.items, status.items);
    assert_eq!(resumed.operations, status.operations);
}

/// Status resolves a path the way Apply does: a removal under
/// a symlinked directory is never recovered as removed (Apply calls it a
/// conflict), so Status never journals a false `Applied`.
#[test]
fn status_never_recovers_a_removal_under_a_symlinked_directory() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let cwd = Path::new(&cwd);
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), cwd.join("link")).unwrap();
    let remove = RemoveItem {
        operation_id: uuid(),
        path: "link/gone.md".into(),
        expected_pre_hash: hash(b"gone"),
    };
    let writes = vec![("x.md".to_string(), b"x".to_vec(), Expected::Absent)];
    let (request, raw) = mixed_batch(&request, &writes, vec![remove], vec![], vec![], None);
    upload_any(root.path(), &request, &raw);
    die_at(root.path(), &request, BatchOperation::Apply, "after-seal");
    let status = call(root.path(), &request, BatchOperation::Status).unwrap();
    assert!(
        !successful(&status.operations[0].outcome),
        "{:?}",
        status.operations
    );
    let resumed = call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert!(matches!(
        resumed.operations[0].outcome,
        MemberOutcome::Effect {
            outcome: EffectOutcome::Conflict { .. }
        }
    ));
}

/// The budget with the exclude block's two receipted writes,
/// with a case-folded removal (its own commit before any write), and for a
/// resumed call.
#[test]
fn the_budget_holds_with_an_exclude_block_a_folded_removal_and_a_resume() {
    let (root, cwd, request, _) = fixture_batch(Codec::Identity);
    let cwd = Path::new(&cwd);
    std::fs::write(cwd.join("Readme.md"), b"old").unwrap();
    let writes: Vec<_> = (0..10)
        .map(|index| {
            (
                format!("d/f{index}.md"),
                format!("f{index}").into_bytes(),
                Expected::Absent,
            )
        })
        .chain([("README.md".to_string(), b"new".to_vec(), Expected::Absent)])
        .collect();
    let block = ManagedExcludeBlock {
        operation_id: uuid(),
        workspace_id: "ws-alpha".into(),
        expected: vec![],
        paths: writes.iter().map(|(path, _, _)| path.clone()).collect(),
        repair: false,
    };
    let remove = RemoveItem {
        operation_id: uuid(),
        path: "Readme.md".into(),
        expected_pre_hash: hash(b"old"),
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
    let meter = durable::Meter::start();
    let applied = call(root.path(), &request, BatchOperation::Apply).unwrap();
    let syncs = meter.read();
    assert!(applied.items.iter().all(|item| successful(&item.outcome)));
    // W = 11 writes; D = the worktree and d/ (2); the folded removal's own
    // commit (+2 flushes, +1 order for its directory); the exclude block's
    // two receipted writes (EXCLUDE_SYNCS each, flushes).
    let (writes, directories) = (11u64, 2u64);
    assert!(syncs.flushes <= 4 + 2 + 2 * EXCLUDE_SYNCS, "{syncs:?}");
    assert!(syncs.orders <= writes + directories + 1, "{syncs:?}");
    assert_eq!(
        journal_names(root.path(), &request).len(),
        2,
        "folded commit + P3"
    );
    // A resumed call (its reply was lost) settles nothing new: 0 syncs.
    let meter = durable::Meter::start();
    call(root.path(), &request, BatchOperation::Apply).unwrap();
    assert_eq!(meter.read().total(), 0);
}
/// Each receipted exclude write, all flushes: its first receipt (file and
/// directory, 2), the staged write and its directory (2), its settled receipt
/// (2), and the `.git/info` directory when the first write creates it (1).
/// Measured: 11 for the two writes (the add skips its unchanged settle).
const EXCLUDE_SYNCS: u64 = 7;
