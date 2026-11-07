// SPDX-License-Identifier: MIT
//! One workspace's long operation never blocks another workspace, nor
//! the same workspace's lock-free reads (head, export, inspect/read/enumerate),
//! nor a PTY spawn.
//!
//! Barrier-based: the long operation parks at a test hold *while holding
//! whatever it holds there*, and the probe must complete while it is still
//! parked. Nothing asserts wall-clock time; [`HANG_GUARD`] only turns a probe
//! that is blocked behind the held operation into a failure instead of a hang.
use super::context_batch::tests::{call as batch_call, fixture_batch, upload as batch_upload};
use super::tests::{fixture, prepared};
use super::*;
use codesense_pty_core::context_batch::{BatchOperation, BatchRequest, Codec};
use codesense_pty_core::remote_export::{
    ExportIntent, ExportOperation, ExportReply, ExportRequest,
};
use codesense_pty_core::remote_files::{
    FileOperation, FileReply, FileRequest, FileTarget, Observation,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

/// Deadlock-to-failure guard, not a performance bound.
const HANG_GUARD: Duration = Duration::from_secs(30);

/// A long operation parked at a test hold on its own thread.
pub(crate) struct Held {
    root: tempfile::TempDir,
    worktree: String,
    release: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<String>>,
    finished: Arc<AtomicBool>,
}

impl Held {
    pub(crate) fn root(&self) -> &Path {
        self.root.path()
    }
    pub(crate) fn worktree(&self) -> &str {
        &self.worktree
    }
    /// True while the held operation has not returned.
    pub(crate) fn still_held(&self) -> bool {
        !self.finished.load(Ordering::SeqCst)
    }
    /// Let the held operation finish; its own result, as text. The fixture
    /// root stays until `self` drops.
    pub(crate) fn release(&mut self) -> String {
        drop(self.release.take());
        self.thread.take().unwrap().join().unwrap()
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn hold(
    point: &'static str,
    root: tempfile::TempDir,
    worktree: String,
    run: impl FnOnce(&Path) -> String + Send + 'static,
) -> Held {
    let (entered, parked) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let finished = Arc::new(AtomicBool::new(false));
    let path = root.path().to_path_buf();
    let done = finished.clone();
    let thread = std::thread::spawn(move || {
        arm_test_hold(point, move || {
            entered.send(()).unwrap();
            let _ = released.recv();
        });
        let result = run(&path);
        done.store(true, Ordering::SeqCst);
        result
    });
    parked
        .recv_timeout(HANG_GUARD)
        .unwrap_or_else(|_| panic!("the long operation never reached its `{point}` hold"));
    Held {
        root,
        worktree,
        release: Some(release),
        thread: Some(thread),
        finished,
    }
}

/// Run `probe` while `held` stays parked: it must finish before the held
/// operation is let go.
pub(crate) fn within_hold<T: Send + 'static>(
    held: &Held,
    what: &str,
    probe: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (sent, result) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sent.send(probe());
    });
    let value = result
        .recv_timeout(HANG_GUARD)
        .unwrap_or_else(|_| panic!("{what} waited on the held operation"));
    assert!(
        held.still_held(),
        "{what} finished only after the held operation"
    );
    value
}

fn target_of(request: &PrepareWorkspace, handle: String) -> FileTarget {
    FileTarget {
        owner_id: 41,
        origin_key: request.origin_key.clone(),
        request_id: request.request_id.clone(),
        handle,
    }
}

/// W2 is prepared; W1 (a second worktree of the same repository) is parked
/// inside `prepare`, holding everything prepare holds.
pub(crate) fn w1_preparing_beside_w2() -> (Held, FileTarget, String, String) {
    let (temp, w2) = fixture();
    let ready = prepared(temp.path(), w2.clone());
    assert_eq!(ready.status, "prepared", "{ready:?}");
    let w2_target = target_of(&w2, ready.handle.unwrap());
    let mut w1 = w2.clone();
    w1.request_id = "00000000-0000-4000-8000-000000000011".into();
    w1.branch = "w1".into();
    w1.worktree_path = temp
        .path()
        .canonicalize()
        .unwrap()
        .join("w1")
        .to_string_lossy()
        .into_owned();
    w1.bundle.size = 0;
    w1.bundle.digest = digest(&[]);
    let w1_worktree = w1.worktree_path.clone();
    let held = hold("prepare", temp, w1_worktree, move |root| {
        prepare(root, 41, w1, || true).status
    });
    (held, w2_target, w2.worktree_path, w2.bundle.commit)
}

/// W1 is parked inside a batch Apply at `point`.
pub(crate) fn w1_applying(point: &'static str) -> (Held, BatchRequest) {
    let (root, cwd, request, encoded) = fixture_batch(Codec::Identity);
    batch_upload(root.path(), &request, &encoded);
    let apply = request.clone();
    let held = hold(point, root, cwd, move |root| {
        match batch_call(root, &apply, BatchOperation::Apply) {
            Ok(status) => format!("{:?}", status.phase),
            Err(error) => error,
        }
    });
    (held, request)
}

fn inspect(root: &Path, target: &FileTarget, path: &str) -> Result<Observation, String> {
    match files::handle(
        root,
        41,
        FileRequest {
            target: target.clone(),
            operation: FileOperation::Inspect { path: path.into() },
        },
        || true,
    )? {
        FileReply::Inspected { observation } => Ok(observation),
        other => Err(format!("{other:?}")),
    }
}

/// W1 is in a long prepare; W2's head and export finish first.
#[test]
fn head_and_export_of_another_workspace_finish_while_one_prepares() {
    let (mut held, w2, _, commit) = w1_preparing_beside_w2();
    let root = held.root().to_path_buf();
    let target = w2.clone();
    let head = within_hold(&held, "W2 WorkspaceHead", move || {
        export::head(&root, 41, &target, || true)
    })
    .unwrap();
    assert_eq!(head.commit, commit);
    let root = held.root().to_path_buf();
    let intent = ExportIntent {
        target: w2,
        request_id: uuid::Uuid::new_v4().to_string(),
        expected_head: commit.clone(),
        prerequisite: None,
    };
    let exported = within_hold(&held, "W2 WorkspaceExport", move || {
        export::handle(
            &root,
            41,
            ExportRequest {
                intent,
                operation: ExportOperation::Prepare,
            },
            || true,
        )
    })
    .unwrap();
    assert!(
        matches!(exported, ExportReply::Prepared { ref descriptor } if descriptor.bundle.commit == commit),
        "{exported:?}"
    );
    // The held prepare keeps its own guarantees once let go.
    assert_eq!(held.release(), "prepared");
}

/// W1's long batch Apply is mid-write of a member; W1's own head finishes
/// first and reports the commit (an Apply never moves HEAD, so pre == post).
#[test]
fn head_of_a_workspace_finishes_during_its_own_long_apply() {
    let (mut held, request) = w1_applying("member-write");
    let root = held.root().to_path_buf();
    let target = request.target.clone();
    let commit = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(held.worktree())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let head = within_hold(&held, "W1 WorkspaceHead", move || {
        export::head(&root, 41, &target, || true)
    })
    .unwrap();
    assert_eq!(head.commit, commit);
    assert_eq!(held.release(), "Applying");
}

/// Never torn: between two members of W1's Apply, reads finish without
/// waiting and see every file either before or after its write.
#[test]
fn reads_during_a_long_apply_see_each_file_before_or_after() {
    let (mut held, request) = w1_applying("between-members");
    let root = held.root().to_path_buf();
    let target = request.target.clone();
    let (first, second) = within_hold(&held, "W1 Inspect", move || {
        (
            inspect(&root, &target, "nested/context/0.md"),
            inspect(&root, &target, "nested/context/1.md"),
        )
    });
    assert!(
        matches!(first, Ok(Observation::Regular { hash, .. }) if hash == *blake3::hash(b"first selected file").as_bytes()),
        "{first:?}"
    );
    assert_eq!(second, Ok(Observation::Absent));
    let root = held.root().to_path_buf();
    let target = request.target.clone();
    let listed = within_hold(&held, "W1 Enumerate", move || {
        files::handle(
            &root,
            41,
            FileRequest {
                target,
                operation: FileOperation::Enumerate {
                    prefix: Some("nested".into()),
                    limit: 16,
                },
            },
            || true,
        )
    });
    assert!(
        matches!(&listed, Ok(FileReply::Paths { paths }) if paths == &["nested/context", "nested/context/0.md"]),
        "{listed:?}"
    );
    let root = held.root().to_path_buf();
    assert_eq!(held.release(), "Applying");
    assert!(matches!(
        inspect(&root, &request.target, "nested/context/1.md"),
        Ok(Observation::Regular { hash, .. }) if hash == *blake3::hash(b"second selected file").as_bytes()
    ));
}

/// No process-wide operations mutex remains in this crate.
#[test]
fn no_global_operations_mutex_remains() {
    let needles = [
        concat!("static ", "OPERATIONS"),
        concat!("OPERATIONS", ".lock"),
    ];
    let mut stack = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut scanned = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                scanned += 1;
                let text = std::fs::read_to_string(&path).unwrap();
                for needle in needles {
                    assert!(
                        !text.contains(needle),
                        "{} still has `{needle}`",
                        path.display()
                    );
                }
            }
        }
    }
    assert!(scanned > 10, "the crate source was scanned");
}

/// Review finding 1: the clone's shared exclude block has its own short lock,
/// so another worktree's exclude write never waits on a 30-minute prepare
/// (which holds the repository lock).
#[test]
fn another_worktrees_exclude_write_finishes_while_one_prepares() {
    use codesense_pty_core::remote_files::EffectOutcome;
    let (mut held, w2, _, _) = w1_preparing_beside_w2();
    let root = held.root().to_path_buf();
    let reply = within_hold(&held, "W2 ReplaceManagedExclude", move || {
        let FileReply::ManagedExclude { expected, .. } = files::handle(
            &root,
            41,
            FileRequest {
                target: w2.clone(),
                operation: FileOperation::ReadManagedExclude,
            },
            || true,
        )?
        else {
            return Err("no exclude state".to_string());
        };
        files::handle(
            &root,
            41,
            FileRequest {
                target: w2,
                operation: FileOperation::ReplaceManagedExclude {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    expected,
                    lines: vec!["docs/".into()],
                },
            },
            || true,
        )
    })
    .unwrap();
    assert!(
        matches!(
            reply,
            FileReply::Effect {
                outcome: EffectOutcome::Applied { .. }
            }
        ),
        "{reply:?}"
    );
    assert_eq!(held.release(), "prepared");
}

/// A late chunk of a batch races the Begin that supersedes it: Begin cleans
/// the old batch only once that chunk's staging lock is free, and the chunk
/// (which arrived after its batch was abandoned) is refused. The old staging
/// is gone afterwards and the new batch stages.
#[test]
fn a_late_chunk_and_the_superseding_begin_never_overlap() {
    use codesense_pty_core::context_batch::{FinishDisposition, TransferPhase};
    let (root, _, first, encoded) = fixture_batch(Codec::Identity);
    batch_call(root.path(), &first, first.operation.clone()).unwrap();
    batch_call(
        root.path(),
        &first,
        BatchOperation::Finish {
            disposition: FinishDisposition::Abandon,
        },
    )
    .unwrap();
    let second = BatchRequest {
        batch_id: uuid::Uuid::new_v4().to_string(),
        ..first.clone()
    };
    let late = first.clone();
    let chunk = encoded.clone();
    let mut held = hold(
        "batch-chunk",
        root,
        String::new(),
        move |root| match batch_call(
            root,
            &late,
            BatchOperation::Chunk {
                offset: 0,
                bytes: chunk,
            },
        ) {
            Ok(_) => "staged".into(),
            Err(error) => error,
        },
    );
    // Begin runs while the late chunk holds its batch's staging lock; it must
    // finish only after that chunk does.
    let path = held.root().to_path_buf();
    let finished = held.finished.clone();
    let begin = std::thread::spawn(move || {
        let result = batch_call(&path, &second, second.operation.clone());
        (result, finished.load(Ordering::SeqCst))
    });
    let late_result = held.release();
    let (begun, chunk_was_done) = begin.join().unwrap();
    assert!(
        chunk_was_done,
        "Begin cleaned the old batch under a live chunk"
    );
    assert_eq!(begun.unwrap().phase, TransferPhase::Uploading);
    assert_ne!(
        late_result, "staged",
        "an abandoned batch's chunk was staged"
    );
}

/// Review finding 4: a record is created by a no-replace rename, so it never
/// has two links (which lock-free readers would call unsafe); a second creator
/// keeps the first one's bytes; no staged file is left behind either way.
#[test]
fn records_are_published_with_one_link_and_one_winner() {
    let temp = tempfile::tempdir().unwrap();
    let dir = File::open(temp.path()).unwrap();
    assert!(publish_new(&dir, "record.json", b"first").unwrap());
    assert!(!publish_new(&dir, "record.json", b"second").unwrap());
    assert_eq!(read(&dir, "record.json").unwrap(), b"first");
    let meta = std::fs::metadata(temp.path().join("record.json")).unwrap();
    assert_eq!(meta.nlink(), 1);
    let names: Vec<_> = std::fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["record.json"], "no staged file remains");
    write_new(&dir, "staged", b"x").unwrap();
    assert_eq!(
        rename_no_replace(&dir, "staged", "record.json").unwrap(),
        NoReplace::Exists
    );
}
