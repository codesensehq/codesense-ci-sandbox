// SPDX-License-Identifier: MIT
//! Desktop tests: Step 1 progress for every phase, a failure event for
//! every phase, and `update_workspace_base` over a fake machine.
use super::*;
use crate::mcp::remote_workspace::{dispatch_with, Step1Phase};
use crate::mcp::remote_workspace_bundle::build_reporting;
use codesense_pty_core::remote_workspace::pipeline::ChunkLink;
use codesense_pty_core::remote_workspace::{
    chunk_range, BundleChunk, BundleUploadStatus, PreparedWorkspace, ReceivedChunks,
    PIPELINED_CHUNK_BYTES,
};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct FakeMachine {
    old_server: bool,
    held: RefCell<Option<ReceivedChunks>>,
    staged: RefCell<BTreeMap<u32, Vec<u8>>>,
    submitted: RefCell<Vec<u32>>,
    max_in_flight: Cell<usize>,
    fail_at: Option<&'static str>,
    knowledge: RepositoryKnowledge,
    precheck: Option<WorkspaceBaseUpdate>,
    updates: RefCell<Vec<UpdateWorkspaceBase>>,
    seals: Cell<usize>,
    /// The workspace the machine already holds for the request.
    holds: Option<PreparedWorkspace>,
    /// Every peer call, by name.
    called: RefCell<Vec<&'static str>>,
}

impl FakeMachine {
    fn failing(at: &'static str) -> Self {
        Self {
            fail_at: Some(at),
            ..Default::default()
        }
    }
    fn fail(&self, at: &str) -> Result<(), String> {
        if self.fail_at == Some(at) {
            Err(format!("{at} failed on the machine"))
        } else {
            Ok(())
        }
    }
    fn uploaded(&self) -> Vec<u8> {
        self.staged.borrow().values().flatten().copied().collect()
    }
}

struct FakePipe<'a> {
    machine: &'a FakeMachine,
    in_flight: Vec<u32>,
}

impl ChunkLink for FakePipe<'_> {
    fn submit(&mut self, index: u32, bytes: Vec<u8>) -> Result<(), String> {
        self.machine.fail("chunk")?;
        self.machine.staged.borrow_mut().insert(index, bytes);
        self.machine.submitted.borrow_mut().push(index);
        self.in_flight.push(index);
        let peak = self.machine.max_in_flight.get().max(self.in_flight.len());
        self.machine.max_in_flight.set(peak);
        Ok(())
    }
    fn next_ack(&mut self) -> Result<u32, String> {
        // Newest first: acks out of order.
        self.in_flight
            .pop()
            .ok_or_else(|| "nothing in flight".into())
    }
}

impl BundlePeer for FakeMachine {
    fn reconcile(&self, _: PrepareWorkspace) -> Result<Option<PreparedWorkspace>, String> {
        self.called.borrow_mut().push("reconcile");
        self.fail("reconcile")?;
        Ok(self.holds.clone())
    }
    fn pipelined(&self) -> bool {
        !self.old_server
    }
    fn upload_status(&self, request: PrepareWorkspace) -> Result<BundleUploadStatus, String> {
        self.called.borrow_mut().push("upload_status");
        self.fail("status")?;
        Ok(BundleUploadStatus {
            chunk_bytes: PIPELINED_CHUNK_BYTES as u32,
            received: self.held.borrow().clone().unwrap_or_else(|| {
                let mut held = ReceivedChunks::for_size(request.bundle.size);
                self.staged.borrow().keys().for_each(|i| held.insert(*i));
                held
            }),
            sealed: false,
        })
    }
    fn chunk_pipe(&self, _: PrepareWorkspace) -> Box<dyn ChunkLink + '_> {
        self.called.borrow_mut().push("chunk_pipe");
        Box::new(FakePipe {
            machine: self,
            in_flight: Vec::new(),
        })
    }
    fn upload_chunk(&self, _: BundleChunk) -> Result<u64, String> {
        Err("this fake is a pipelined machine".into())
    }
    fn seal(&self, _: PrepareWorkspace) -> Result<(), String> {
        self.called.borrow_mut().push("seal");
        self.fail("seal")?;
        self.seals.set(self.seals.get() + 1);
        Ok(())
    }
    fn prepare(&self, request: PrepareWorkspace) -> PreparedWorkspace {
        self.called.borrow_mut().push("prepare");
        if self.fail("prepare").is_err() {
            return PreparedWorkspace::failure("refused", "worktree destination is occupied");
        }
        PreparedWorkspace {
            status: "prepared".into(),
            handle: Some("remote-workspace-x".into()),
            cwd: Some(request.worktree_path),
            repo_path: Some(request.repo_path),
            repository_key: Some(request.repository_key),
            resolved_commit: Some(request.bundle.commit),
            branch: Some(request.branch),
            message: None,
        }
    }
}

impl BasePeer for FakeMachine {
    fn supports_base_update(&self) -> bool {
        !self.old_server
    }
    fn inspect(&self, _: InspectRepository) -> Result<RepositoryKnowledge, String> {
        self.fail("probe")?;
        Ok(self.knowledge.clone())
    }
    fn update_base(&self, request: UpdateWorkspaceBase) -> Result<WorkspaceBaseUpdate, String> {
        self.updates.borrow_mut().push(request.clone());
        if request.precheck {
            return Ok(self.precheck.clone().unwrap_or(WorkspaceBaseUpdate {
                status: "ready".into(),
                ..Default::default()
            }));
        }
        self.fail("update")?;
        Ok(WorkspaceBaseUpdate {
            status: "updated".into(),
            previous_commit: self
                .precheck
                .as_ref()
                .and_then(|p| p.previous_commit.clone()),
            commit: Some(request.upload.bundle.commit),
            ..Default::default()
        })
    }
}

fn recorder() -> (ProgressSink, Arc<Mutex<Vec<PrepareProgress>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    (
        Arc::new(move |event| seen.lock().unwrap().push(event)),
        events,
    )
}

/// Each fixture file: incompressible and distinct, so a bundle's size counts
/// the files it carries.
const FILE: usize = 200 * 1024;

fn random_bytes(len: usize, seed: u32) -> Vec<u8> {
    let mut random = 11u32.wrapping_add(seed.wrapping_mul(2_654_435_761)) | 1;
    (0..len)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random as u8
        })
        .collect()
}

fn request_for(bytes: &[u8]) -> PrepareWorkspace {
    PrepareWorkspace {
        request_id: "00000000-0000-4000-8000-000000000001".into(),
        owner_id: 41,
        origin_key: "a".repeat(64),
        repository_key: "github.com/acme/repo".into(),
        clone_url: "https://github.com/acme/repo.git".into(),
        repo_path: "/home/dev/codesense/repos/acme/repo".into(),
        worktree_path: "/home/dev/codesense/worktrees/t/SW-1".into(),
        base_ref: "main".into(),
        branch: "story/sw-1".into(),
        bundle: GitBundle {
            size: bytes.len() as u64,
            digest: format!("{:x}", Sha256::digest(bytes)),
            commit: "c".repeat(40),
        },
    }
}

/// A git repository with an `origin` and `n` commits on `main`, each adding a
/// file of random bytes.
fn seed_repo(dir: &std::path::Path, commits: usize) -> (git2::Repository, String, Vec<git2::Oid>) {
    let repo = git2::Repository::init(dir).unwrap();
    let url = "https://github.com/example/base.git";
    repo.remote("origin", url).unwrap();
    let key = crate::context_repo::canonical_remote_key(url).unwrap();
    let ids = add_commits(&repo, commits, 0);
    repo.set_head("refs/heads/main").unwrap();
    (repo, key, ids)
}

/// One commit adding `n` small files: past Git's unpack limit, so a fetch of
/// it reports "Receiving objects" counts.
fn add_many(repo: &git2::Repository, n: usize) {
    let sig = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
    let parent = repo
        .find_reference("refs/heads/main")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    let mut builder = repo.treebuilder(Some(&parent.tree().unwrap())).unwrap();
    for i in 0..n {
        let blob = repo.blob(format!("small file {i}\n").as_bytes()).unwrap();
        builder
            .insert(format!("small-{i}.txt"), blob, 0o100644)
            .unwrap();
    }
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    repo.commit(
        Some("refs/heads/main"),
        &sig,
        &sig,
        "many",
        &tree,
        &[&parent],
    )
    .unwrap();
}

fn add_commits(repo: &git2::Repository, commits: usize, from: usize) -> Vec<git2::Oid> {
    let sig = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
    let mut ids = Vec::new();
    for n in from..from + commits {
        let parent = repo
            .find_reference("refs/heads/main")
            .ok()
            .and_then(|r| r.peel_to_commit().ok());
        let mut builder = repo
            .treebuilder(parent.as_ref().map(|c| c.tree().unwrap()).as_ref())
            .unwrap();
        let blob = repo.blob(&random_bytes(FILE, n as u32)).unwrap();
        builder
            .insert(format!("file-{n}.bin"), blob, 0o100644)
            .unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        ids.push(
            repo.commit(
                Some("refs/heads/main"),
                &sig,
                &sig,
                &format!("commit {n}"),
                &tree,
                &parents,
            )
            .unwrap(),
        );
    }
    ids
}

/// Every remote Step 1 phase emits progress, with determinate counts
/// where available; over the fake, a resume sends only the holes.
#[test]
fn xs9_remote_step1_reports_every_phase_with_counts() {
    let temp = tempfile::tempdir().unwrap();
    let seed = temp.path().join("seed");
    let (repo, key, _) = seed_repo(&seed, 3);
    add_many(&repo, 150);
    let (sink, events) = recorder();
    let (path, bundle) = build_reporting(
        temp.path(),
        "k",
        "k",
        BuildSource {
            url: "https://github.com/example/base.git".into(),
            repository_key: key,
            base_ref: "main".into(),
            seed: Some(seed),
        },
        || true,
        |_| Ok(RepositoryKnowledge::default()),
        &*sink,
    )
    .unwrap();
    assert!(bundle.size > 3 * PIPELINED_CHUNK_BYTES as u64 / 2);
    let mut request = request_for(&std::fs::read(&path).unwrap());
    request.bundle = bundle.clone();
    let machine = FakeMachine::default();
    let mut held = ReceivedChunks::for_size(bundle.size);
    held.insert(0);
    *machine.held.borrow_mut() = Some(held.clone());
    let result = dispatch_with(&machine, request, &path, &|| Ok(()), &*sink).unwrap();
    assert_eq!(result.status, "prepared");
    assert_eq!(
        *machine.submitted.borrow(),
        held.missing(),
        "only the holes"
    );
    assert_eq!(machine.seals.get(), 1);

    let events = events.lock().unwrap().clone();
    let fetching: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            PrepareProgress::Fetching { objects, total } => Some((*objects, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(
        fetching.first(),
        Some(&(None, None)),
        "Fetching starts at once"
    );
    assert!(
        fetching
            .iter()
            .any(|(done, total)| done.is_some() && total.is_some_and(|t| t > 0)),
        "Git's object counts reach Fetching: {fetching:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PrepareProgress::Bundling { objects: Some(n) } if *n > 0)),
        "Bundling carries the object count: {events:?}"
    );
    let first_copy = chunk_range(bundle.size, 0).unwrap().1 as u64;
    let copies: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            PrepareProgress::Copying { sent, total } => Some((*sent, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(
        copies.first(),
        Some(&(first_copy, bundle.size)),
        "resume counts held bytes"
    );
    assert_eq!(copies.last(), Some(&(bundle.size, bundle.size)));
    let order: Vec<Step1Phase> =
        events
            .iter()
            .filter_map(PrepareProgress::phase)
            .fold(Vec::new(), |mut seen, phase| {
                if seen.last() != Some(&phase) {
                    seen.push(phase);
                }
                seen
            });
    assert_eq!(
        order,
        vec![
            Step1Phase::Fetching,
            Step1Phase::Bundling,
            Step1Phase::Copying,
            Step1Phase::Verifying,
            Step1Phase::CreatingWorktree,
        ]
    );
}

/// Every Step 1 phase that fails emits a failure event naming it.
#[test]
fn xs9_every_step1_phase_failure_emits_a_failure_event() {
    let temp = tempfile::tempdir().unwrap();
    let seed = temp.path().join("seed");
    let (_repo, key, _) = seed_repo(&seed, 2);
    let source = |seed: Option<PathBuf>| BuildSource {
        url: "https://github.com/example/base.git".into(),
        repository_key: key.clone(),
        base_ref: "main".into(),
        seed,
    };
    // Each run goes through `report_step1`, the composition the real
    // `prepare_reporting` is: phases through its tracked sink, then settled.
    let run = |n: usize, seed: Option<PathBuf>, machine: FakeMachine| -> Vec<PrepareProgress> {
        let (sink, events) = recorder();
        let machine = &machine;
        let source = source(seed);
        let root = temp.path();
        let result = tauri::async_runtime::block_on(crate::mcp::remote_workspace::report_step1(
            sink,
            |tracked, consumed| async move {
                let (path, bundle) = build_reporting(
                    root,
                    &format!("run-{n}"),
                    "i",
                    source,
                    || true,
                    |commit| {
                        machine.inspect(InspectRepository {
                            owner_id: 41,
                            repository_key: String::new(),
                            clone_url: String::new(),
                            repo_path: String::new(),
                            commit: commit.into(),
                        })
                    },
                    &*tracked,
                )?;
                consumed.store(true, std::sync::atomic::Ordering::SeqCst);
                let mut request = request_for(&[1]);
                request.bundle = bundle;
                dispatch_with(machine, request, &path, &|| Ok(()), &*tracked)
            },
        ));
        assert!(matches!(
            result.status.as_str(),
            "prepared" | "refused" | "unknown"
        ));
        let events = events.lock().unwrap().clone();
        events
    };
    let failed = |events: &[PrepareProgress]| match events.last() {
        Some(PrepareProgress::Failed {
            failed_phase,
            message,
        }) => {
            assert!(!message.is_empty());
            Some(*failed_phase)
        }
        _ => None,
    };
    let seed = Some(seed);
    assert_eq!(
        failed(&run(0, None, FakeMachine::default())),
        Some(Step1Phase::Fetching),
        "no local objects for the base"
    );
    assert_eq!(
        failed(&run(1, seed.clone(), FakeMachine::failing("probe"))),
        Some(Step1Phase::Bundling)
    );
    assert_eq!(
        failed(&run(2, seed.clone(), FakeMachine::failing("chunk"))),
        Some(Step1Phase::Copying)
    );
    assert_eq!(
        failed(&run(3, seed.clone(), FakeMachine::failing("seal"))),
        Some(Step1Phase::Verifying)
    );
    assert_eq!(
        failed(&run(4, seed.clone(), FakeMachine::failing("prepare"))),
        Some(Step1Phase::CreatingWorktree)
    );
    let ok = run(5, seed, FakeMachine::default());
    assert_eq!(failed(&ok), None, "a success emits no failure");
    // The payload the UI reads.
    assert_eq!(
        serde_json::to_value(PrepareProgress::Failed {
            failed_phase: Step1Phase::Verifying,
            message: "bundle digest mismatch".into()
        })
        .unwrap(),
        serde_json::json!({"phase":"failed","failedPhase":"verifying","message":"bundle digest mismatch"})
    );
}

fn plan(previous: &str) -> BasePlan {
    let mut prepared = request_for(&[1]);
    prepared.bundle.commit = previous.into();
    BasePlan {
        target: FileTarget {
            owner_id: 41,
            origin_key: prepared.origin_key.clone(),
            request_id: prepared.request_id.clone(),
            handle: "remote-workspace-x".into(),
        },
        prepared,
        update_id: "00000000-0000-4000-8000-0000000000aa".into(),
        remove_managed_copies: false,
        managed: Vec::new(),
        discard: Vec::new(),
    }
}

const NO_HOOKS: UpdateHooks<'static> = UpdateHooks {
    begun: &|| {},
    settled: &|| {},
};

/// A clean worktree with 3 new base commits: only the delta is sent
/// (its prerequisite is the commit the machine has) and HEAD advances.
#[test]
fn xs9_update_workspace_base_sends_only_the_delta_when_clean() {
    let temp = tempfile::tempdir().unwrap();
    let seed = temp.path().join("seed");
    let (repo, key, first) = seed_repo(&seed, 2);
    let base = first.last().unwrap().to_string();
    let newer = add_commits(&repo, 3, 2);
    let tip = newer.last().unwrap().to_string();
    let machine = FakeMachine {
        knowledge: RepositoryKnowledge {
            has_commit: false,
            imported_commits: vec![base.clone()],
        },
        precheck: Some(WorkspaceBaseUpdate {
            status: "ready".into(),
            previous_commit: Some(base.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let plan = plan(&base);
    let (sink, events) = recorder();
    let root = temp.path().join("bundles");
    let build: Box<BuildBundle<'_>> = Box::new(|probe, progress| {
        build_reporting(
            &root,
            "base-1",
            "base-1",
            BuildSource {
                url: "https://github.com/example/base.git".into(),
                repository_key: key.clone(),
                base_ref: "main".into(),
                seed: Some(seed.clone()),
            },
            || true,
            probe,
            progress,
        )
    });
    let (answer, sent) =
        update_with(&machine, &plan, build, &|| Ok(()), &*sink, &NO_HOOKS).unwrap();
    assert_eq!(answer.status, "updated");
    assert_eq!(
        answer.commit.as_deref(),
        Some(tip.as_str()),
        "HEAD advances"
    );
    let uploaded = machine.uploaded();
    assert_eq!(uploaded.len() as u64, sent);
    assert!(
        uploaded
            .windows(41)
            .any(|w| w == format!("-{base}").as_bytes()),
        "the delta names the machine's commit as its prerequisite"
    );
    // Only the 3 new commits' files travel, not the 2 the machine has.
    assert!(sent > 3 * FILE as u64, "delta {sent} bytes");
    assert!(sent < 4 * FILE as u64, "delta {sent} bytes");
    let updates = machine.updates.borrow();
    assert_eq!(updates.len(), 2);
    assert!(updates[0].precheck && !updates[1].precheck);
    assert_eq!(updates[1].upload.bundle.commit, tip);
    assert_eq!(updates[1].upload.request_id, plan.update_id);
    assert_eq!(updates[1].upload.worktree_path, plan.prepared.worktree_path);
    let events = events.lock().unwrap().clone();
    assert!(events.contains(&PrepareProgress::Verifying));
    assert_eq!(events.last(), Some(&PrepareProgress::UpdatingWorktree));
}

/// A dirty worktree is refused before anything is built or sent.
#[test]
fn xs9_update_workspace_base_refuses_dirty_before_building_or_sending() {
    let machine = FakeMachine {
        precheck: Some(WorkspaceBaseUpdate {
            status: "refusedDirty".into(),
            dirty: vec![" M src/lib.rs".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let plan = plan(&"b".repeat(40));
    let build: Box<BuildBundle<'_>> =
        Box::new(|_, _| panic!("nothing is built for a dirty worktree"));
    let (answer, sent) =
        update_with(&machine, &plan, build, &|| Ok(()), &|_| {}, &NO_HOOKS).unwrap();
    assert_eq!(answer.status, "refusedDirty");
    assert_eq!(answer.dirty, vec![" M src/lib.rs".to_string()]);
    assert_eq!(sent, 0);
    assert!(machine.submitted.borrow().is_empty());
    assert_eq!(machine.updates.borrow().len(), 1, "only the precheck");
}

/// Nothing new on the base: no transfer, no second call.
#[test]
fn xs9_update_workspace_base_is_a_no_op_when_already_current() {
    let temp = tempfile::tempdir().unwrap();
    let seed = temp.path().join("seed");
    let (_repo, key, first) = seed_repo(&seed, 1);
    let base = first[0].to_string();
    let machine = FakeMachine {
        knowledge: RepositoryKnowledge {
            has_commit: true,
            imported_commits: vec![base.clone()],
        },
        precheck: Some(WorkspaceBaseUpdate {
            status: "ready".into(),
            previous_commit: Some(base.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let root = temp.path().join("bundles");
    let build: Box<BuildBundle<'_>> = Box::new(|probe, progress| {
        build_reporting(
            &root,
            "base-2",
            "base-2",
            BuildSource {
                url: "https://github.com/example/base.git".into(),
                repository_key: key.clone(),
                base_ref: "main".into(),
                seed: Some(seed.clone()),
            },
            || true,
            probe,
            progress,
        )
    });
    let (answer, sent) = update_with(
        &machine,
        &plan(&base),
        build,
        &|| Ok(()),
        &|_| {},
        &NO_HOOKS,
    )
    .unwrap();
    assert_eq!(answer.status, "upToDate");
    assert_eq!(sent, 0);
    assert_eq!(machine.updates.borrow().len(), 1);
}

#[test]
fn xs9_update_workspace_base_needs_a_current_terminal_server() {
    let machine = FakeMachine {
        old_server: true,
        ..Default::default()
    };
    let build: Box<BuildBundle<'_>> = Box::new(|_, _| unreachable!());
    let error = update_with(
        &machine,
        &plan(&"b".repeat(40)),
        build,
        &|| Ok(()),
        &|_| {},
        &NO_HOOKS,
    )
    .unwrap_err();
    assert!(error.contains("terminal server is too old"));
}

/// The update reports on the workspace's own channel, keyed by
/// the resolved handle; a refusal emits a failure event; a precheck refusal
/// keeps nothing and a final answer drops every local trace.
#[test]
fn xs9_run_update_reports_by_handle_and_settles_on_final_answers() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let payloads = Arc::new(Mutex::new(Vec::new()));
    let seen = payloads.clone();
    let emit: Arc<dyn Fn(BaseProgressPayload) + Send + Sync> =
        Arc::new(move |payload| seen.lock().unwrap().push(payload));
    let (begun, settled) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let hooks = UpdateHooks {
        begun: &|| {
            begun.fetch_add(1, Ordering::SeqCst);
        },
        settled: &|| {
            settled.fetch_add(1, Ordering::SeqCst);
        },
    };
    let dirty = FakeMachine {
        precheck: Some(WorkspaceBaseUpdate {
            status: "refusedDirty".into(),
            message: Some("uncommitted changes".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (answer, _) = run_update(
        &dirty,
        &plan(&"b".repeat(40)),
        Box::new(|_, _| unreachable!()),
        &|| Ok(()),
        emit.clone(),
        &hooks,
    );
    assert_eq!(answer.status, "refusedDirty");
    assert_eq!(
        *payloads.lock().unwrap(),
        vec![BaseProgressPayload {
            workspace_handle: "remote-workspace-x".into(),
            progress: PrepareProgress::Failed {
                failed_phase: Step1Phase::Starting,
                message: "uncommitted changes".into(),
            },
        }]
    );
    assert_eq!(
        serde_json::to_value(&payloads.lock().unwrap()[0]).unwrap(),
        serde_json::json!({"workspaceHandle":"remote-workspace-x","phase":"failed","failedPhase":"starting","message":"uncommitted changes"})
    );
    assert_eq!(
        begun.load(Ordering::SeqCst),
        0,
        "a refused precheck keeps nothing"
    );
    assert_eq!(settled.load(Ordering::SeqCst), 0);
}

/// An update interrupted after its upload resumes under the
/// same id: nothing is sent again and the second attempt finishes it.
#[test]
fn xs9_an_interrupted_update_resumes_its_upload_under_the_same_id() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let temp = tempfile::tempdir().unwrap();
    let seed = temp.path().join("seed");
    let (repo, key, first) = seed_repo(&seed, 1);
    let base = first[0].to_string();
    let tip = add_commits(&repo, 3, 1).last().unwrap().to_string();
    let ids = UpdateIds {
        dir: temp.path().join("base-updates"),
    };
    let handle = "remote-workspace-x";
    let mut plan = plan(&base);
    plan.update_id = ids.for_target(handle, &tip);
    let root = temp.path().join("bundles");
    let bundle_key = format!("base-{}", plan.update_id);
    let builder = || -> Box<BuildBundle<'_>> {
        let (root, key, bundle_key, seed) =
            (root.clone(), key.clone(), bundle_key.clone(), seed.clone());
        Box::new(move |probe, progress| {
            build_reporting(
                &root,
                &bundle_key,
                &bundle_key,
                BuildSource {
                    url: "https://github.com/example/base.git".into(),
                    repository_key: key,
                    base_ref: "main".into(),
                    seed: Some(seed),
                },
                || true,
                probe,
                progress,
            )
        })
    };
    let settled = AtomicUsize::new(0);
    let hooks = UpdateHooks {
        begun: &|| ids.keep(handle, &tip, &plan.update_id).unwrap(),
        settled: &|| {
            settled.fetch_add(1, Ordering::SeqCst);
            bundle::discard(&root, &bundle_key, &key).unwrap();
            ids.forget(handle);
        },
    };
    let machine = FakeMachine {
        fail_at: Some("update"),
        knowledge: RepositoryKnowledge {
            has_commit: false,
            imported_commits: vec![base.clone()],
        },
        precheck: Some(WorkspaceBaseUpdate {
            status: "ready".into(),
            previous_commit: Some(base.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let lost = update_with(&machine, &plan, builder(), &|| Ok(()), &|_| {}, &hooks).unwrap_err();
    assert!(lost.contains("update failed"), "{lost}");
    let sent_once = machine.submitted.borrow().len();
    assert!(sent_once > 0);
    assert_eq!(
        settled.load(Ordering::SeqCst),
        0,
        "an unknown outcome keeps its upload"
    );
    assert_eq!(
        ids.for_target(handle, &tip),
        plan.update_id,
        "the retry reuses the id"
    );
    assert!(root.join(format!("bundle-{bundle_key}")).exists());

    let machine = FakeMachine {
        staged: machine.staged,
        knowledge: machine.knowledge,
        precheck: machine.precheck,
        ..Default::default()
    };
    let (answer, _) = update_with(&machine, &plan, builder(), &|| Ok(()), &|_| {}, &hooks).unwrap();
    assert_eq!(answer.status, "updated");
    assert!(
        machine.submitted.borrow().is_empty(),
        "nothing was sent again"
    );
    assert_eq!(settled.load(Ordering::SeqCst), 1);
    // Every local trace is gone: the record, the bundle dir and its cache ref.
    assert_ne!(ids.for_target(handle, &tip), plan.update_id);
    assert!(!root.join(format!("bundle-{bundle_key}")).exists());
    let cache = root
        .join(format!("repository-{:x}", Sha256::digest(key.as_bytes())))
        .join("repository.git");
    let cache = git2::Repository::open_bare(cache).unwrap();
    assert!(cache
        .find_reference(&format!("refs/codesense/requests/{bundle_key}"))
        .is_err());
}

#[test]
fn xs9_update_ids_are_per_workspace_and_target() {
    let temp = tempfile::tempdir().unwrap();
    let ids = UpdateIds {
        dir: temp.path().join("base-updates"),
    };
    let first = ids.for_target("w", "c1");
    assert_ne!(ids.for_target("w", "c1"), first, "not kept until begun");
    ids.keep("w", "c1", &first).unwrap();
    assert_eq!(ids.for_target("w", "c1"), first);
    assert_ne!(
        ids.for_target("w", "c2"),
        first,
        "a newer target is a new update"
    );
    assert_ne!(ids.for_target("other", "c1"), first);
    ids.forget("w");
    assert_ne!(ids.for_target("w", "c1"), first);
    // Re-review A: a newer target abandons the unfinished update, by id.
    ids.keep("w", "c1", &first).unwrap();
    let (next, abandoned) = ids.resume("w", "c2");
    assert_ne!(next, first);
    assert_eq!(abandoned, Some(first.clone()));
    assert_eq!(ids.resume("w", "c1"), (first, None));
}

/// Re-review A: the abandoned id reaches the machine with the precheck.
#[test]
fn xs9_an_abandoned_update_id_is_sent_for_discard() {
    let machine = FakeMachine {
        precheck: Some(WorkspaceBaseUpdate {
            status: "refusedDirty".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut plan = plan(&"b".repeat(40));
    plan.discard = vec!["00000000-0000-4000-8000-0000000000ab".into()];
    let build: Box<BuildBundle<'_>> = Box::new(|_, _| unreachable!());
    update_with(&machine, &plan, build, &|| Ok(()), &|_| {}, &NO_HOOKS).unwrap();
    assert_eq!(
        machine.updates.borrow()[0].discard_uploads,
        vec!["00000000-0000-4000-8000-0000000000ab".to_string()]
    );
}

/// Re-opening a workspace the machine already holds is
/// one reconcile call and 0 bytes transferred — no upload status, no chunk,
/// no seal, no prepare — however large the bundle.
#[test]
fn xs12_budget_step1_reopen_transfers_zero_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let seed = temp.path().join("seed");
    let (repo, key, _) = seed_repo(&seed, 3);
    add_many(&repo, 150);
    let (path, bundle) = build_reporting(
        temp.path(),
        "k",
        "k",
        BuildSource {
            url: "https://github.com/example/base.git".into(),
            repository_key: key,
            base_ref: "main".into(),
            seed: Some(seed),
        },
        || true,
        |_| Ok(RepositoryKnowledge::default()),
        &|_| {},
    )
    .unwrap();
    assert!(
        bundle.size > PIPELINED_CHUNK_BYTES as u64,
        "a bundle worth sending"
    );
    let mut request = request_for(&std::fs::read(&path).unwrap());
    request.bundle = bundle;
    let machine = FakeMachine {
        holds: Some(PreparedWorkspace {
            status: "alreadyPrepared".into(),
            handle: Some("remote-workspace-x".into()),
            cwd: Some(request.worktree_path.clone()),
            ..PreparedWorkspace::failure("", "")
        }),
        ..Default::default()
    };
    let result = dispatch_with(&machine, request, &path, &|| Ok(()), &|_| {}).unwrap();
    assert_eq!(result.handle.as_deref(), Some("remote-workspace-x"));
    assert_eq!(*machine.called.borrow(), vec!["reconcile"], "one call");
    assert!(machine.uploaded().is_empty(), "0 bytes transferred");
    assert_eq!(machine.seals.get(), 0);
}
