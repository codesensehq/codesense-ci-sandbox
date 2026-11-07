// SPDX-License-Identifier: MIT
use super::*;
use codesense_pty_core::remote_workspace::GitBundle;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;

/// A prepared export: what was promised, its bytes, and whether it was released.
type Prepared = (GitBundle, Vec<u8>, bool);

/// How a machine misbehaves. The Mac trusts the machine less than itself.
#[derive(Default)]
struct Hostility {
    /// Report this branch (`Some(None)`: detached) instead of the checkout's.
    branch: Option<Option<String>>,
    /// Put these refs (name, commit) in the bundle beside the pinned commit.
    extra_refs: Vec<(String, String)>,
    /// Answer Prepare with a descriptor naming another commit.
    wrong_descriptor: bool,
    /// Build the bundle, then lose the first Prepare reply.
    lose_first_prepare: bool,
}

/// A machine's terminal server reduced to its export contract, over a real Git
/// worktree: `head` reads the checkout, `Prepare` refuses a moved HEAD and
/// packs exactly the pinned commit (minus the prerequisite), `Read` serves
/// bounded chunks, `Release` drops the bytes.
struct GitHost {
    worktree: PathBuf,
    staging: tempfile::TempDir,
    bundles: Mutex<HashMap<String, Prepared>>,
    prerequisites: Mutex<Vec<Option<String>>>,
    releases: Mutex<usize>,
    release_attempts: Mutex<usize>,
    lost: Mutex<bool>,
    hostility: Hostility,
    chunk: usize,
}

impl GitHost {
    fn new(worktree: &Path) -> Self {
        Self::hostile(worktree, Hostility::default())
    }
    fn hostile(worktree: &Path, hostility: Hostility) -> Self {
        Self {
            worktree: worktree.to_path_buf(),
            staging: tempfile::tempdir().unwrap(),
            bundles: Mutex::default(),
            prerequisites: Mutex::default(),
            releases: Mutex::new(0),
            release_attempts: Mutex::new(0),
            lost: Mutex::new(false),
            hostility,
            chunk: 64,
        }
    }
    fn descriptor(&self, id: &str, bundle: &GitBundle) -> ExportDescriptor {
        let mut bundle = bundle.clone();
        if self.hostility.wrong_descriptor {
            bundle.commit = "f".repeat(40);
        }
        ExportDescriptor {
            request_id: id.to_string(),
            bundle,
            released: false,
        }
    }
}

fn refused(message: &str) -> ExportCallError {
    ExportCallError::Refused(message.into())
}

impl ExportHost for GitHost {
    fn head(&self, _target: &FileTarget) -> Result<WorkspaceHead, String> {
        let commit = sh(&self.worktree, &["rev-parse", "HEAD"]);
        let branch = match &self.hostility.branch {
            Some(reported) => reported.clone(),
            None => {
                let current = sh(&self.worktree, &["branch", "--show-current"]);
                (!current.is_empty()).then_some(current)
            }
        };
        Ok(WorkspaceHead { commit, branch })
    }
    fn export(&self, request: ExportRequest) -> Result<ExportReply, ExportCallError> {
        let id = request.intent.request_id.clone();
        match request.operation {
            ExportOperation::Prepare => {
                if let Some((bundle, _, _)) = self.bundles.lock().unwrap().get(&id) {
                    return Ok(ExportReply::Prepared {
                        descriptor: self.descriptor(&id, bundle),
                    });
                }
                if sh(&self.worktree, &["rev-parse", "HEAD"]) != request.intent.expected_head {
                    return Err(refused("managed worktree HEAD differs from expectedHead"));
                }
                if let Some(base) = &request.intent.prerequisite {
                    let ok = Command::new("git")
                        .args([
                            "merge-base",
                            "--is-ancestor",
                            base,
                            &request.intent.expected_head,
                        ])
                        .current_dir(&self.worktree)
                        .status()
                        .unwrap()
                        .success();
                    if !ok {
                        return Err(refused("prerequisite is not an ancestor"));
                    }
                }
                self.prerequisites
                    .lock()
                    .unwrap()
                    .push(request.intent.prerequisite.clone());
                let reference = format!("refs/codesense/exports/{}", id);
                sh(
                    &self.worktree,
                    &["update-ref", &reference, &request.intent.expected_head],
                );
                let mut names = vec![reference.clone()];
                for (name, commit) in &self.hostility.extra_refs {
                    sh(&self.worktree, &["update-ref", name, commit]);
                    names.push(name.clone());
                }
                let path = self.staging.path().join(format!("{id}.bundle"));
                let mut args = vec![
                    "bundle".to_string(),
                    "create".to_string(),
                    path.to_string_lossy().into_owned(),
                ];
                args.extend(names);
                if let Some(base) = &request.intent.prerequisite {
                    args.push(format!("^{base}"));
                }
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                sh(&self.worktree, &refs);
                sh(&self.worktree, &["update-ref", "-d", &reference]);
                let bytes = std::fs::read(&path).unwrap();
                let bundle = GitBundle {
                    digest: format!("{:x}", Sha256::digest(&bytes)),
                    size: bytes.len() as u64,
                    commit: request.intent.expected_head.clone(),
                };
                self.bundles
                    .lock()
                    .unwrap()
                    .insert(id.clone(), (bundle.clone(), bytes, false));
                if self.hostility.lose_first_prepare
                    && !std::mem::replace(&mut *self.lost.lock().unwrap(), true)
                {
                    return Err(ExportCallError::Lost(
                        "the session server did not answer in time".into(),
                    ));
                }
                Ok(ExportReply::Prepared {
                    descriptor: self.descriptor(&id, &bundle),
                })
            }
            ExportOperation::Read { offset } => {
                let bundles = self.bundles.lock().unwrap();
                let (_, bytes, released) = bundles.get(&id).ok_or_else(|| refused("no export"))?;
                if *released {
                    return Err(refused("released"));
                }
                let start = offset as usize;
                let end = (start + self.chunk).min(bytes.len());
                Ok(ExportReply::Chunk {
                    offset,
                    bytes: bytes[start..end].to_vec(),
                })
            }
            ExportOperation::Release { bundle } => {
                *self.release_attempts.lock().unwrap() += 1;
                let mut bundles = self.bundles.lock().unwrap();
                let entry = bundles.get_mut(&id).ok_or_else(|| refused("no export"))?;
                if entry.0 != bundle && !self.hostility.wrong_descriptor {
                    return Err(refused("mismatch"));
                }
                entry.2 = true;
                entry.1.clear();
                *self.releases.lock().unwrap() += 1;
                Ok(ExportReply::Released {
                    descriptor: ExportDescriptor {
                        request_id: id,
                        bundle,
                        released: true,
                    },
                })
            }
        }
    }
}

fn sh(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn commit(dir: &Path, file: &str, text: &str, message: &str) -> String {
    std::fs::write(dir.join(file), text).unwrap();
    sh(dir, &["add", file]);
    sh(dir, &["commit", "-q", "-m", message]);
    sh(dir, &["rev-parse", "HEAD"])
}

struct Fixture {
    _root: tempfile::TempDir,
    clone: PathBuf,
    machine: PathBuf,
    base: String,
}

const BRANCH: &str = "story/sw-1";

/// A local clone with `main` and the story branch at the base commit (HEAD on
/// `main`), and the machine's worktree made from it, on the story branch.
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let clone = root.path().join("clone");
    std::fs::create_dir(&clone).unwrap();
    sh(&clone, &["init", "-q"]);
    let base = commit(&clone, "README.md", "base\n", "base");
    sh(&clone, &["branch", BRANCH]);
    let machine = root.path().join("machine");
    sh(
        root.path(),
        &[
            "clone",
            "-q",
            clone.to_str().unwrap(),
            machine.to_str().unwrap(),
        ],
    );
    sh(&machine, &["checkout", "-q", BRANCH]);
    Fixture {
        _root: root,
        clone,
        machine,
        base,
    }
}

fn target() -> FileTarget {
    FileTarget {
        owner_id: 41,
        origin_key: "a".repeat(64),
        request_id: uuid::Uuid::new_v4().to_string(),
        handle: format!("remote-workspace-{}", "b".repeat(64)),
    }
}

fn run(fixture: &Fixture, host: &GitHost) -> Result<FetchReport, String> {
    run_as(fixture, host, "codesense-test-1", BRANCH)
}

fn run_as(
    fixture: &Fixture,
    host: &GitHost,
    machine_id: &str,
    prepared_branch: &str,
) -> Result<FetchReport, String> {
    run_on_base(fixture, host, machine_id, prepared_branch, None)
}

fn run_on_base(
    fixture: &Fixture,
    host: &GitHost,
    machine_id: &str,
    prepared_branch: &str,
    base_commit: Option<String>,
) -> Result<FetchReport, String> {
    let valid = || -> Result<(), String> { Ok(()) };
    fetch_into_clone(FetchSource {
        host,
        target: target(),
        machine_id: machine_id.into(),
        prepared_commit: fixture.base.clone(),
        base_commit,
        prepared_branch: prepared_branch.into(),
        clone: fixture.clone.clone(),
        valid: &valid,
    })
}

/// After `update_workspace_base` moved the machine to
/// a newer base, a fetch knows that base: it is the bundle's prerequisite and
/// its commits are not reported (or sent) as the machine's work again.
#[test]
fn xs9_a_fetch_after_a_base_update_never_pulls_the_base_delta_again() {
    let fixture = fixture();
    sh(&fixture.clone, &["checkout", "-q", "-B", "main"]);
    let mut newer = String::new();
    for n in 0..3 {
        newer = commit(
            &fixture.clone,
            &format!("base-{n}.txt"),
            "base\n",
            &format!("base {n}"),
        );
    }
    // The machine moved its story branch to the new base (a fast-forward),
    // then an agent committed on it.
    sh(&fixture.machine, &["fetch", "-q", "origin", "main"]);
    sh(&fixture.machine, &["merge", "-q", "--ff-only", &newer]);
    let made = commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");

    let without = GitHost::new(&fixture.machine);
    let report = run_on_base(&fixture, &without, "codesense-test-1", BRANCH, None).unwrap();
    assert_eq!(
        report.commit_count, 4,
        "unaware, the base commits count as arrived"
    );
    assert_eq!(
        without
            .prerequisites
            .lock()
            .unwrap()
            .first()
            .cloned()
            .flatten(),
        Some(fixture.base.clone())
    );

    let fresh = fixture_like(&fixture);
    let host = GitHost::new(&fixture.machine);
    let report = run_on_base(
        &fresh,
        &host,
        "codesense-test-1",
        BRANCH,
        Some(newer.clone()),
    )
    .unwrap();
    assert_eq!(
        host.prerequisites
            .lock()
            .unwrap()
            .first()
            .cloned()
            .flatten(),
        Some(newer),
        "only what came after the machine's base is sent"
    );
    assert_eq!(
        report.commits,
        vec![ArrivedCommit {
            sha: made,
            subject: "agent work".into()
        }]
    );
    assert_eq!(report.commit_count, 1);
}

/// A second local clone of the same fixture with no fetched refs yet.
fn fixture_like(fixture: &Fixture) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let clone = root.path().join("clone");
    sh(
        root.path(),
        &[
            "clone",
            "-q",
            "--no-local",
            "--single-branch",
            "--branch",
            "main",
            fixture.clone.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    sh(&clone, &["branch", "-q", "-f", BRANCH, &fixture.base]);
    Fixture {
        _root: root,
        clone,
        machine: fixture.machine.clone(),
        base: fixture.base.clone(),
    }
}

/// Every ref of `repo` and what it points at.
fn all_refs(repo: &Path) -> String {
    sh(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
}

#[test]
fn a_commit_made_on_the_machine_lands_in_the_local_clone_and_fast_forwards() {
    let fixture = fixture();
    let made = commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");
    let host = GitHost::new(&fixture.machine);

    let report = run(&fixture, &host).unwrap();
    assert_eq!(report.branch, BRANCH);
    assert_eq!(report.commit, made);
    assert_eq!(
        report.remote_ref,
        format!("refs/remotes/codesense-machine-codesense-test-1/{BRANCH}")
    );
    assert_eq!(report.previous_remote_ref, None);
    assert_eq!(
        report.commits,
        vec![ArrivedCommit {
            sha: made.clone(),
            subject: "agent work".into()
        }]
    );
    assert_eq!(report.commit_count, 1);
    assert_eq!(report.local_branch_state, LocalBranch::FastForwarded);
    assert!(report.fast_forwarded);
    assert_eq!(report.reason, None);
    assert_eq!(sh(&fixture.clone, &["rev-parse", &report.remote_ref]), made);
    assert_eq!(
        sh(
            &fixture.clone,
            &["rev-parse", &format!("refs/heads/{BRANCH}")]
        ),
        made
    );
    assert_eq!(
        sh(&fixture.clone, &["show", &format!("{made}:agent.txt")]),
        "agent work"
    );
    // Only what came after the workspace's starting commit crossed, and the
    // machine was told to drop the bundle.
    assert_eq!(
        *host.prerequisites.lock().unwrap(),
        vec![Some(fixture.base.clone())]
    );
    assert_eq!(*host.releases.lock().unwrap(), 1);
    // The checkout on main is untouched.
    assert_eq!(sh(&fixture.clone, &["rev-parse", "HEAD"]), fixture.base);

    // A repeat with nothing new sends nothing and reports nothing new.
    let again = run(&fixture, &host).unwrap();
    assert_eq!(again.local_branch_state, LocalBranch::UpToDate);
    assert!(again.commits.is_empty());
    assert_eq!(again.previous_remote_ref.as_deref(), Some(made.as_str()));
    assert_eq!(host.prerequisites.lock().unwrap().len(), 1);

    // The next commit is packed against the previous fetch.
    let next = commit(&fixture.machine, "agent.txt", "more\n", "more work");
    let third = run(&fixture, &host).unwrap();
    assert_eq!(third.commits.len(), 1);
    assert_eq!(third.commits[0].sha, next);
    assert!(third.fast_forwarded);
    assert_eq!(
        host.prerequisites
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .as_deref(),
        Some(made.as_str())
    );
}

#[test]
fn a_diverged_local_branch_is_left_alone_and_reported() {
    let fixture = fixture();
    let made = commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");
    sh(&fixture.clone, &["checkout", "-q", BRANCH]);
    let local = commit(&fixture.clone, "local.txt", "local\n", "local work");
    sh(&fixture.clone, &["checkout", "-q", "main"]);
    let host = GitHost::new(&fixture.machine);

    let report = run(&fixture, &host).unwrap();
    assert_eq!(report.local_branch_state, LocalBranch::NotFastForwarded);
    assert!(!report.fast_forwarded);
    assert_eq!(report.reason, Some(NotFastForwarded::Diverged));
    assert!(report.message.unwrap().contains(&report.remote_ref));
    assert_eq!(report.commits.len(), 1);
    assert_eq!(sh(&fixture.clone, &["rev-parse", &report.remote_ref]), made);
    assert_eq!(
        sh(
            &fixture.clone,
            &["rev-parse", &format!("refs/heads/{BRANCH}")]
        ),
        local
    );
}

#[test]
fn a_checked_out_branch_moves_only_when_its_checkout_is_clean() {
    let fixture = fixture();
    let made = commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");
    sh(&fixture.clone, &["checkout", "-q", BRANCH]);
    std::fs::write(fixture.clone.join("README.md"), "edited\n").unwrap();
    let host = GitHost::new(&fixture.machine);

    let dirty = run(&fixture, &host).unwrap();
    assert_eq!(dirty.reason, Some(NotFastForwarded::DirtyCheckout));
    assert_eq!(sh(&fixture.clone, &["rev-parse", "HEAD"]), fixture.base);
    assert_eq!(
        std::fs::read_to_string(fixture.clone.join("README.md")).unwrap(),
        "edited\n"
    );

    sh(&fixture.clone, &["checkout", "-q", "--", "README.md"]);
    let clean = run(&fixture, &host).unwrap();
    assert_eq!(clean.local_branch_state, LocalBranch::FastForwarded);
    assert_eq!(sh(&fixture.clone, &["rev-parse", "HEAD"]), made);
    assert_eq!(
        std::fs::read_to_string(fixture.clone.join("agent.txt")).unwrap(),
        "agent work\n"
    );
}

#[test]
fn a_branch_checked_out_in_another_worktree_is_not_moved() {
    let fixture = fixture();
    commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");
    let other = fixture.clone.parent().unwrap().join("other-worktree");
    sh(
        &fixture.clone,
        &["worktree", "add", "-q", other.to_str().unwrap(), BRANCH],
    );
    let host = GitHost::new(&fixture.machine);

    let report = run(&fixture, &host).unwrap();
    assert_eq!(report.reason, Some(NotFastForwarded::CheckedOutElsewhere));
    assert_eq!(
        sh(
            &fixture.clone,
            &["rev-parse", &format!("refs/heads/{BRANCH}")]
        ),
        fixture.base
    );
}

#[test]
fn a_missing_local_branch_is_created_and_a_detached_checkout_is_refused() {
    let fixture = fixture();
    sh(&fixture.clone, &["branch", "-D", BRANCH]);
    let made = commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");
    let host = GitHost::new(&fixture.machine);
    let report = run(&fixture, &host).unwrap();
    assert_eq!(report.local_branch_state, LocalBranch::Created);
    assert_eq!(
        sh(
            &fixture.clone,
            &["rev-parse", &format!("refs/heads/{BRANCH}")]
        ),
        made
    );

    sh(&fixture.machine, &["checkout", "-q", "--detach"]);
    let error = run(&fixture, &host).unwrap_err();
    assert!(error.contains("detached"), "{error}");
}

#[test]
fn a_rewritten_machine_branch_is_fetched_without_touching_local_work() {
    let fixture = fixture();
    let first = commit(&fixture.machine, "agent.txt", "one\n", "one");
    let host = GitHost::new(&fixture.machine);
    run(&fixture, &host).unwrap();
    sh(&fixture.machine, &["reset", "-q", "--hard", &fixture.base]);
    let rewritten = commit(&fixture.machine, "agent.txt", "two\n", "two");
    let report = run(&fixture, &host).unwrap();
    assert_eq!(report.previous_remote_ref.as_deref(), Some(first.as_str()));
    assert_eq!(
        sh(&fixture.clone, &["rev-parse", &report.remote_ref]),
        rewritten
    );
    assert_eq!(report.reason, Some(NotFastForwarded::Diverged));
    assert_eq!(
        sh(
            &fixture.clone,
            &["rev-parse", &format!("refs/heads/{BRANCH}")]
        ),
        first
    );
}

#[test]
fn an_outdated_terminal_server_is_refused_by_name() {
    let message = crate::terminal::session_server::WorkspaceHeadError::OutdatedServer.to_string();
    assert!(message.contains("update the terminal server"), "{message}");
}

#[test]
fn a_story_or_workspace_id_resolves_to_its_machine_and_handle() {
    let handle = format!("remote-workspace-{}", "c".repeat(64));
    let viewer = serde_json::json!({"workspaces": {
        "acme:default": [
            {"id": "local-ws", "storyId": "SW-2", "cwd": "/Users/me/wt"},
            {"id": "ws-1", "storyId": "SW-1", "cwd": "",
             "location": {"kind": "machine", "machineId": "box-1", "requestId": "r", "handle": handle}},
            {"id": "ws-3", "storyId": "SW-3", "cwd": "",
             "location": {"kind": "machine", "machineId": "box-1", "requestId": "r"}}
        ],
        "acme:other": [
            {"id": "ws-9", "storyId": "SW-1", "cwd": "",
             "location": {"kind": "machine", "machineId": "box-2", "requestId": "r", "handle": "remote-workspace-other"}}
        ]
    }});
    let by = |params: RemoteWorkspaceFetchParams| locate_in_viewer(&viewer, &params);
    assert_eq!(
        by(RemoteWorkspaceFetchParams {
            workspace_id: Some("ws-1".into()),
            ..Default::default()
        })
        .unwrap(),
        ("box-1".to_string(), handle.clone())
    );
    assert!(by(RemoteWorkspaceFetchParams {
        story_id: Some("SW-1".into()),
        ..Default::default()
    })
    .unwrap_err()
    .contains("teamKey"));
    assert_eq!(
        by(RemoteWorkspaceFetchParams {
            story_id: Some("SW-1".into()),
            team_key: Some("acme:default".into()),
            ..Default::default()
        })
        .unwrap(),
        ("box-1".to_string(), handle)
    );
    assert!(by(RemoteWorkspaceFetchParams {
        story_id: Some("SW-2".into()),
        ..Default::default()
    })
    .unwrap_err()
    .contains("this Mac"));
    assert!(by(RemoteWorkspaceFetchParams {
        story_id: Some("SW-3".into()),
        ..Default::default()
    })
    .unwrap_err()
    .contains("still being created"));
    assert!(by(RemoteWorkspaceFetchParams::default()).is_err());
}

/// A machine-made commit on top of local `main`, which a hostile machine
/// would like the Mac to fast-forward `main` onto.
fn commit_on_top_of_main(fixture: &Fixture) -> String {
    commit(&fixture.machine, "evil.txt", "evil\n", "evil")
}

#[test]
fn a_machine_naming_another_branch_is_refused_and_nothing_is_written() {
    let fixture = fixture();
    commit_on_top_of_main(&fixture);
    let before = all_refs(&fixture.clone);
    for reported in ["main", "-x", "a..b", "refs/heads/main", ""] {
        let host = GitHost::hostile(
            &fixture.machine,
            Hostility {
                branch: Some(Some(reported.into())),
                ..Default::default()
            },
        );
        let error = run(&fixture, &host).unwrap_err();
        assert!(
            error.contains("not the workspace's branch"),
            "{reported}: {error}"
        );
        assert!(
            host.prerequisites.lock().unwrap().is_empty(),
            "nothing was exported"
        );
        assert_eq!(all_refs(&fixture.clone), before, "{reported}");
    }
    for prepared in ["-x", "a..b", "refs/heads/main", "main.lock", ""] {
        let host = GitHost::new(&fixture.machine);
        assert!(
            run_as(&fixture, &host, "codesense-test-1", prepared).is_err(),
            "{prepared}"
        );
        assert_eq!(all_refs(&fixture.clone), before);
    }
}

#[test]
fn refs_a_bundle_carries_beside_the_commit_are_never_written() {
    let fixture = fixture();
    let made = commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");
    let evil = sh(
        &fixture.machine,
        &[
            "commit-tree",
            "-p",
            &fixture.base,
            "-m",
            "evil",
            &format!("{}^{{tree}}", fixture.base),
        ],
    );
    let host = GitHost::hostile(
        &fixture.machine,
        Hostility {
            extra_refs: vec![
                ("refs/heads/main".into(), evil.clone()),
                ("refs/tags/evil".into(), evil.clone()),
                ("refs/remotes/origin/main".into(), evil),
            ],
            ..Default::default()
        },
    );
    let before = all_refs(&fixture.clone);
    let report = run(&fixture, &host).unwrap();
    assert!(report.fast_forwarded);
    let after = all_refs(&fixture.clone);
    let changed: Vec<&str> = after
        .lines()
        .filter(|line| !before.lines().any(|old| old == *line))
        .collect();
    assert_eq!(
        changed,
        vec![
            format!("refs/heads/{BRANCH} {made}").as_str(),
            format!("{} {made}", report.remote_ref).as_str(),
        ]
    );
    assert_eq!(
        sh(&fixture.clone, &["rev-parse", "refs/heads/main"]),
        fixture.base
    );
}

#[test]
fn malformed_objects_are_refused_before_anything_moves_and_the_bundle_is_released() {
    let fixture = fixture();
    std::fs::write(
        fixture.machine.join(".gitmodules"),
        "[submodule \"sub\"]\n\tpath = sub\n\turl = --upload-pack=touch /tmp/pwned\n",
    )
    .unwrap();
    sh(&fixture.machine, &["add", ".gitmodules"]);
    sh(
        &fixture.machine,
        &["commit", "-q", "-m", "hostile submodule"],
    );
    let host = GitHost::new(&fixture.machine);
    let before = all_refs(&fixture.clone);
    let error = run(&fixture, &host).unwrap_err();
    assert!(error.contains("object checks"), "{error}");
    assert_eq!(all_refs(&fixture.clone), before);
    assert_eq!(*host.release_attempts.lock().unwrap(), 1);
    assert_eq!(*host.releases.lock().unwrap(), 1);
}

#[test]
fn every_prepared_bundle_is_released_whatever_fails_after_it() {
    let fixture = fixture();
    commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");
    let before = all_refs(&fixture.clone);

    let wrong = GitHost::hostile(
        &fixture.machine,
        Hostility {
            wrong_descriptor: true,
            ..Default::default()
        },
    );
    assert!(run(&fixture, &wrong)
        .unwrap_err()
        .contains("different export"));
    assert_eq!(*wrong.release_attempts.lock().unwrap(), 1);

    let lost = GitHost::hostile(
        &fixture.machine,
        Hostility {
            lose_first_prepare: true,
            ..Default::default()
        },
    );
    assert!(run(&fixture, &lost).unwrap_err().contains("did not answer"));
    assert_eq!(
        *lost.releases.lock().unwrap(),
        1,
        "the lost export was found and released"
    );
    assert_eq!(all_refs(&fixture.clone), before);

    // The same machine, answering properly, still brings the work home.
    assert!(
        run(&fixture, &GitHost::new(&fixture.machine))
            .unwrap()
            .fast_forwarded
    );
}

#[test]
fn a_branch_being_rebased_or_bisected_anywhere_is_not_moved() {
    for (worktree, file, text) in [
        (
            false,
            "rebase-merge/head-name",
            format!("refs/heads/{BRANCH}\n"),
        ),
        (false, "BISECT_START", format!("{BRANCH}\n")),
        (
            true,
            "rebase-apply/head-name",
            format!("refs/heads/{BRANCH}\n"),
        ),
    ] {
        let fixture = fixture();
        commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");
        let gitdir = if worktree {
            let other = fixture.clone.parent().unwrap().join("rebasing");
            sh(
                &fixture.clone,
                &["worktree", "add", "-q", "--detach", other.to_str().unwrap()],
            );
            fixture.clone.join(".git/worktrees/rebasing")
        } else {
            fixture.clone.join(".git")
        };
        let path = gitdir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        let report = run(&fixture, &GitHost::new(&fixture.machine)).unwrap();
        assert_eq!(
            report.reason,
            Some(NotFastForwarded::CheckedOutElsewhere),
            "{file}"
        );
        assert_eq!(
            sh(
                &fixture.clone,
                &["rev-parse", &format!("refs/heads/{BRANCH}")]
            ),
            fixture.base,
            "{file}"
        );
    }
}

#[test]
fn a_machine_called_origin_never_touches_the_real_origin() {
    let fixture = fixture();
    let made = commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");
    let origin = format!("refs/remotes/origin/{BRANCH}");
    sh(&fixture.clone, &["update-ref", &origin, &fixture.base]);
    let report = run_as(&fixture, &GitHost::new(&fixture.machine), "origin", BRANCH).unwrap();
    assert_eq!(
        report.remote_ref,
        format!("refs/remotes/codesense-machine-origin/{BRANCH}")
    );
    assert_eq!(sh(&fixture.clone, &["rev-parse", &origin]), fixture.base);
    assert_eq!(sh(&fixture.clone, &["rev-parse", &report.remote_ref]), made);
    for lossy in ["a.b", "a/b", "", "..", "x y"] {
        assert!(
            run_as(&fixture, &GitHost::new(&fixture.machine), lossy, BRANCH).is_err(),
            "{lossy}"
        );
    }
}

#[test]
fn a_fetched_ref_holding_an_annotated_tag_is_still_moved_by_compare_and_set() {
    let fixture = fixture();
    let made = commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");
    sh(
        &fixture.clone,
        &["tag", "-a", "-m", "tag", "base-tag", &fixture.base],
    );
    let tag = sh(&fixture.clone, &["rev-parse", "refs/tags/base-tag"]);
    let remote_ref = format!("refs/remotes/codesense-machine-codesense-test-1/{BRANCH}");
    sh(&fixture.clone, &["update-ref", &remote_ref, &tag]);
    let report = run(&fixture, &GitHost::new(&fixture.machine)).unwrap();
    assert_eq!(
        report.previous_remote_ref.as_deref(),
        Some(fixture.base.as_str())
    );
    assert_eq!(sh(&fixture.clone, &["rev-parse", &remote_ref]), made);
}

#[test]
fn a_checkout_that_moved_off_the_branch_is_not_fast_forwarded() {
    let fixture = fixture();
    let local_ref = format!("refs/heads/{BRANCH}");
    // HEAD is on main: the checkout of the branch went away after it was seen.
    let before = all_refs(&fixture.clone);
    let outcome = fast_forward_checkout(
        &fixture.clone,
        BRANCH,
        &local_ref,
        "refs/remotes/x",
        &fixture.base,
    )
    .unwrap();
    assert_eq!(outcome.1, Some(NotFastForwarded::CheckoutChanged));
    assert_eq!(all_refs(&fixture.clone), before);
}

/// A clone checked out on the story branch, a machine commit on that branch
/// already fetched into it, and `main` holding a file of its own.
fn checked_out_with_a_fetched_commit() -> (Fixture, String) {
    let fixture = fixture();
    commit(&fixture.clone, "main.txt", "main only\n", "main work");
    sh(&fixture.clone, &["checkout", "-q", BRANCH]);
    let made = commit(&fixture.machine, "agent.txt", "agent work\n", "agent work");
    sh(
        &fixture.clone,
        &[
            "fetch",
            "-q",
            fixture.machine.to_str().unwrap(),
            &format!("{BRANCH}:refs/remotes/x"),
        ],
    );
    (fixture, made)
}

fn try_git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn a_switch_just_before_the_update_leaves_the_other_branch_untouched() {
    let (fixture, made) = checked_out_with_a_fetched_commit();
    let local_ref = format!("refs/heads/{BRANCH}");
    let main_before = sh(&fixture.clone, &["rev-parse", "main"]);

    let outcome = fast_forward_checkout_with(
        &fixture.clone,
        BRANCH,
        &local_ref,
        "refs/remotes/x",
        &made,
        |step| {
            if step == CheckoutStep::BeforeLock {
                sh(&fixture.clone, &["checkout", "-q", "main"]);
            }
        },
    )
    .unwrap();

    assert_eq!(outcome.1, Some(NotFastForwarded::CheckoutChanged));
    assert_eq!(
        sh(&fixture.clone, &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(sh(&fixture.clone, &["rev-parse", "main"]), main_before);
    assert_eq!(sh(&fixture.clone, &["rev-parse", &local_ref]), fixture.base);
    assert_eq!(sh(&fixture.clone, &["status", "--porcelain"]), "");
    assert!(!fixture.clone.join("agent.txt").exists());
    assert!(fixture.clone.join("main.txt").exists());
}

#[test]
fn no_switch_can_run_while_the_checkout_is_updated() {
    let (fixture, made) = checked_out_with_a_fetched_commit();
    let local_ref = format!("refs/heads/{BRANCH}");
    let main_before = sh(&fixture.clone, &["rev-parse", "main"]);
    let mut switched = None;

    let outcome = fast_forward_checkout_with(
        &fixture.clone,
        BRANCH,
        &local_ref,
        "refs/remotes/x",
        &made,
        |step| {
            if step == CheckoutStep::Locked {
                switched = Some(try_git(&fixture.clone, &["checkout", "-q", "main"]));
            }
        },
    )
    .unwrap();

    assert_eq!(switched, Some(false), "the index lock must block a switch");
    assert_eq!(outcome.0, LocalBranch::FastForwarded);
    assert_eq!(sh(&fixture.clone, &["symbolic-ref", "HEAD"]), local_ref);
    assert_eq!(sh(&fixture.clone, &["rev-parse", "HEAD"]), made);
    assert_eq!(sh(&fixture.clone, &["rev-parse", "main"]), main_before);
    assert_eq!(sh(&fixture.clone, &["status", "--porcelain"]), "");
    assert!(fixture.clone.join("agent.txt").exists());
    assert!(!fixture.clone.join("main.txt").exists());
    let gitdir = fixture.clone.join(".git");
    let leftovers: Vec<_> = std::fs::read_dir(&gitdir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("index."))
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
}

#[test]
fn an_untracked_file_in_the_way_blocks_the_update_and_nothing_moves() {
    let (fixture, made) = checked_out_with_a_fetched_commit();
    let local_ref = format!("refs/heads/{BRANCH}");
    std::fs::write(fixture.clone.join("agent.txt"), "mine\n").unwrap();

    let outcome =
        fast_forward_checkout(&fixture.clone, BRANCH, &local_ref, "refs/remotes/x", &made).unwrap();

    assert_eq!(outcome.1, Some(NotFastForwarded::DirtyCheckout));
    assert_eq!(sh(&fixture.clone, &["rev-parse", &local_ref]), fixture.base);
    assert_eq!(
        std::fs::read_to_string(fixture.clone.join("agent.txt")).unwrap(),
        "mine\n"
    );
    assert!(!fixture.clone.join(".git/index.lock").exists());
}

#[test]
fn a_failed_index_write_puts_the_branch_and_files_back() {
    let (fixture, made) = checked_out_with_a_fetched_commit();
    let local_ref = format!("refs/heads/{BRANCH}");
    let lock = fixture.clone.join(".git/index.lock");

    let outcome = fast_forward_checkout_with(
        &fixture.clone,
        BRANCH,
        &local_ref,
        "refs/remotes/x",
        &made,
        |step| {
            if step == CheckoutStep::BeforeCommit {
                // The index can't be written: the lock file vanished.
                std::fs::remove_file(&lock).unwrap();
            }
        },
    );

    assert!(outcome.is_err(), "{outcome:?}");
    assert_eq!(sh(&fixture.clone, &["rev-parse", &local_ref]), fixture.base);
    assert_eq!(sh(&fixture.clone, &["status", "--porcelain"]), "");
    assert!(!fixture.clone.join("agent.txt").exists());
}

#[test]
fn a_crashed_fetchs_scratch_index_is_cleaned_and_a_stale_lock_is_reported_not_taken() {
    let (fixture, made) = checked_out_with_a_fetched_commit();
    let local_ref = format!("refs/heads/{BRANCH}");
    let gitdir = fixture.clone.join(".git");
    std::fs::write(gitdir.join("index.codesense-fetch-deadbeef"), b"stale").unwrap();
    std::fs::write(gitdir.join("index.lock"), b"").unwrap();

    let blocked =
        fast_forward_checkout(&fixture.clone, BRANCH, &local_ref, "refs/remotes/x", &made).unwrap();
    assert_eq!(blocked.1, Some(NotFastForwarded::CheckoutChanged));
    assert!(blocked.2.unwrap().contains("index.lock"));
    assert!(
        gitdir.join("index.lock").exists(),
        "Git's lock was taken over"
    );

    std::fs::remove_file(gitdir.join("index.lock")).unwrap();
    let moved =
        fast_forward_checkout(&fixture.clone, BRANCH, &local_ref, "refs/remotes/x", &made).unwrap();
    assert_eq!(moved.0, LocalBranch::FastForwarded);
    assert!(!gitdir.join("index.codesense-fetch-deadbeef").exists());
}

#[tokio::test]
async fn move_carries_unpushed_machine_branch_into_a_mac_worktree_at_the_exact_tip() {
    let fixture = fixture();
    let tip = commit(
        &fixture.machine,
        "private.txt",
        "work stays private",
        "unpushed work",
    );
    let host = GitHost::new(&fixture.machine);
    let fetched = run(&fixture, &host).unwrap();
    assert_eq!(fetched.commit, tip);
    let ws = crate::mcp::move_workspace::WorkspaceSnapshot {
        machine_id: crate::mcp::move_workspace::MachineRef(None),
        request_id: String::new(),
        handle: None,
        cwd: None,
        source_repo: crate::domain::SourceRepoSlug::new("example", "work").unwrap(),
        base_ref: format!("refs/heads/{BRANCH}"),
        branch: BRANCH.into(),
        repo_path: fixture.clone.to_string_lossy().into_owned(),
        worktree_path: String::new(),
    };
    let target = fixture._root.path().join("mac-target");
    let prepared =
        crate::mcp::move_workspace::local_prepare_workspace(&ws, target.to_str().unwrap())
            .await
            .unwrap();
    assert_eq!(prepared.resolved_commit.as_deref(), Some(tip.as_str()));
    assert_eq!(
        sh(
            Path::new(prepared.cwd.as_deref().unwrap()),
            &["rev-parse", "HEAD"]
        ),
        tip
    );
    assert!(fixture.machine.join("private.txt").exists());
}
