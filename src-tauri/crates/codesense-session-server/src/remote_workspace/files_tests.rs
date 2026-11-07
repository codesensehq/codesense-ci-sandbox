// SPDX-License-Identifier: MIT
use super::*;
use crate::remote_workspace::tests::{fixture, fixture_with_history, prepared};
use codesense_host_proto::test_support::strip_attribution_env;
use std::os::unix::fs::symlink;

fn workspace() -> (tempfile::TempDir, FileTarget, String, String) {
    workspace_from(fixture())
}
fn workspace_from(
    (root, request): (tempfile::TempDir, PrepareWorkspace),
) -> (tempfile::TempDir, FileTarget, String, String) {
    let result = prepared(root.path(), request.clone());
    assert_eq!(result.status, "prepared", "{result:?}");
    let target = FileTarget {
        owner_id: 41,
        origin_key: request.origin_key,
        request_id: request.request_id,
        handle: result.handle.unwrap(),
    };
    (root, target, request.worktree_path, request.repo_path)
}
fn call(root: &Path, target: &FileTarget, operation: FileOperation) -> FileReply {
    handle(
        root,
        41,
        FileRequest {
            target: target.clone(),
            operation,
        },
        || true,
    )
    .unwrap()
}
fn op() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn write(id: &str, path: &str, expected: Expected, bytes: &[u8]) -> FileOperation {
    FileOperation::Apply {
        operation_id: id.into(),
        path: path.into(),
        expected,
        effect: Effect::Write {
            bytes: bytes.to_vec(),
            mode: Some(0o640),
        },
    }
}
fn is_applied(reply: FileReply) -> bool {
    matches!(
        reply,
        FileReply::Effect {
            outcome: EffectOutcome::Applied { .. }
        }
    )
}

#[test]
fn file_effects_preserve_edits_and_reconcile_original_receipt() {
    let (root, target, cwd, _) = workspace();
    assert!(is_applied(call(
        root.path(),
        &target,
        FileOperation::EnsureDirectories {
            operation_id: op(),
            path: "context/selected".into()
        }
    )));
    let id = op();
    let request = write(
        &id,
        "context/selected/story.md",
        Expected::Absent,
        b"locally saved, uncommitted context",
    );
    assert!(is_applied(call(root.path(), &target, request.clone())));
    assert_eq!(
        std::fs::read(format!("{cwd}/context/selected/story.md")).unwrap(),
        b"locally saved, uncommitted context"
    );
    // Simulated acknowledgement loss: same UUID returns the durable outcome.
    assert!(is_applied(call(
        root.path(),
        &target,
        FileOperation::Reconcile {
            operation_id: id.clone()
        }
    )));
    std::fs::write(
        format!("{cwd}/context/selected/story.md"),
        b"agent changed it",
    )
    .unwrap();
    assert!(is_applied(call(root.path(), &target, request)));
    assert_eq!(
        std::fs::read(format!("{cwd}/context/selected/story.md")).unwrap(),
        b"agent changed it"
    );
    let conflict = call(
        root.path(),
        &target,
        write(
            &op(),
            "context/selected/story.md",
            Expected::Hash {
                hash: *blake3::hash(b"locally saved, uncommitted context").as_bytes(),
            },
            b"new source",
        ),
    );
    assert!(matches!(
        conflict,
        FileReply::Effect {
            outcome: EffectOutcome::Conflict { .. }
        }
    ));
    assert!(handle(
        root.path(),
        41,
        FileRequest {
            target: target.clone(),
            operation: write(&id, "other", Expected::Absent, b"changed intent")
        },
        || true
    )
    .is_err());
}
#[test]
fn pending_and_missing_operation_namespaces_do_not_authorize_blind_replay() {
    let (root, target, cwd, _) = workspace();
    let absent_id = op();
    assert!(
        matches!(call(root.path(), &target, FileOperation::Reconcile { operation_id: absent_id.clone() }), FileReply::Effect { outcome: EffectOutcome::NotStarted { operation_id } } if operation_id == absent_id)
    );
    let workspace = resolve(root.path(), 41, &target, &|| true).unwrap();
    let id = op();
    let intent = FileRequest {
        target: target.clone(),
        operation: write(&id, "story.md", Expected::Absent, b"source"),
    };
    save_receipt(
        &workspace.ledger,
        &ledger_key(&target, &id).unwrap(),
        &FileReceipt::new(intent.clone()),
        true,
    )
    .unwrap();
    assert!(matches!(
        call(root.path(), &target, intent.operation),
        FileReply::Effect {
            outcome: EffectOutcome::Unknown { .. }
        }
    ));
    assert!(!Path::new(&cwd).join("story.md").exists());
    // Receipt effect observed after daemon restart can be acknowledged without replay.
    std::fs::write(Path::new(&cwd).join("story.md"), b"source").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        Path::new(&cwd).join("story.md"),
        std::fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    assert!(is_applied(call(
        root.path(),
        &target,
        FileOperation::Reconcile { operation_id: id }
    )));
    std::fs::rename(
        root.path().join("remote-file-operations"),
        root.path().join("lost-ledger"),
    )
    .unwrap();
    assert!(matches!(
        call(
            root.path(),
            &target,
            FileOperation::Reconcile { operation_id: op() }
        ),
        FileReply::Effect {
            outcome: EffectOutcome::Unknown { .. }
        }
    ));
}
#[test]
fn unsafe_paths_fifo_and_symlink_never_escape_or_block() {
    let (root, target, cwd, _) = workspace();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, Path::new(&cwd).join("linked")).unwrap();
    let fifo = CString::new(format!("{cwd}/fifo")).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert_eq!(
        call(
            root.path(),
            &target,
            FileOperation::Inspect {
                path: "fifo".into()
            }
        ),
        FileReply::Inspected {
            observation: Observation::Unsafe
        }
    );
    for path in ["../outside", ".git/config", "linked/escape"] {
        let reply = call(
            root.path(),
            &target,
            write(&op(), path, Expected::Absent, b"no"),
        );
        assert!(!is_applied(reply));
    }
    assert!(!outside.join("escape").exists());
    assert!(matches!(
        handle(
            root.path(),
            41,
            FileRequest {
                target,
                operation: write(&op(), "retired", Expected::Absent, b"no")
            },
            || false
        )
        .unwrap(),
        FileReply::Effect {
            outcome: EffectOutcome::Unknown { .. }
        }
    ));
    assert!(!Path::new(&cwd).join("retired").exists());
}
#[test]
fn enumeration_and_index_are_scoped_complete_or_error() {
    let (root, target, cwd, _) = workspace();
    let cwd = Path::new(&cwd);
    std::fs::create_dir(cwd.join("selected")).unwrap();
    std::fs::create_dir(cwd.join("unrelated")).unwrap();
    std::fs::write(cwd.join("selected/story.md"), "one").unwrap();
    for n in 0..10 {
        std::fs::write(cwd.join(format!("unrelated/{n}")), "other").unwrap();
    }
    assert!(Command::new("git")
        .current_dir(cwd)
        .args(["add", "."])
        .status()
        .unwrap()
        .success());
    assert_eq!(
        call(
            root.path(),
            &target,
            FileOperation::TrackedPaths {
                prefix: Some("selected".into()),
                limit: 1
            }
        ),
        FileReply::Paths {
            paths: vec!["selected/story.md".into()]
        }
    );
    assert_eq!(
        call(
            root.path(),
            &target,
            FileOperation::Enumerate {
                prefix: Some("selected".into()),
                limit: 1
            }
        ),
        FileReply::Paths {
            paths: vec!["selected/story.md".into()]
        }
    );
    assert!(handle(
        root.path(),
        41,
        FileRequest {
            target: target.clone(),
            operation: FileOperation::Enumerate {
                prefix: Some("unrelated".into()),
                limit: 1
            }
        },
        || true
    )
    .is_err());
    assert!(handle(
        root.path(),
        41,
        FileRequest {
            target,
            operation: FileOperation::TrackedPaths {
                prefix: Some("unrelated".into()),
                limit: 1
            }
        },
        || true
    )
    .is_err());
}
fn git_at(cwd: &Path, args: &[&str]) {
    assert!(strip_attribution_env(&mut Command::new("git"))
        .current_dir(cwd)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success());
}

#[test]
fn managed_exclude_preserves_user_bytes_and_conflicts_on_changed_baseline() {
    let (root, target, _, repo) = workspace();
    let exclude = Path::new(&repo).join(".git/info/exclude");
    std::fs::write(
        &exclude,
        b"user-first\r\n# >>> codesense managed\nold\n# <<< codesense managed\nuser-last\r\n",
    )
    .unwrap();
    let FileReply::ManagedExclude { expected, lines } =
        call(root.path(), &target, FileOperation::ReadManagedExclude)
    else {
        panic!("wrong reply")
    };
    assert_eq!(lines, vec!["old"]);
    let id = op();
    assert!(is_applied(call(
        root.path(),
        &target,
        FileOperation::ReplaceManagedExclude {
            operation_id: id.clone(),
            expected: expected.clone(),
            lines: vec!["selected/story.md".into()]
        }
    )));
    assert_eq!(std::fs::read(&exclude).unwrap(), b"user-first\r\n# >>> codesense managed\nselected/story.md\n# <<< codesense managed\nuser-last\r\n");
    assert!(is_applied(call(
        root.path(),
        &target,
        FileOperation::Reconcile { operation_id: id }
    )));
    assert!(matches!(
        call(
            root.path(),
            &target,
            FileOperation::ReplaceManagedExclude {
                operation_id: op(),
                expected,
                lines: vec!["other".into()]
            }
        ),
        FileReply::Effect {
            outcome: EffectOutcome::Conflict { .. }
        }
    ));
}

#[test]
fn equivalent_preparation_receipts_reconcile_stable_handle_without_resetting_namespace() {
    let (root, target, cwd, _) = workspace();
    let operation_id = op();
    assert!(is_applied(call(
        root.path(),
        &target,
        write(&operation_id, "selected.md", Expected::Absent, b"initial")
    )));
    let receipts = directory(
        &File::open(root.path()).unwrap(),
        "remote-workspaces",
        false,
    )
    .unwrap();
    let original_key = format!(
        "{}.json",
        digest(format!("{}:{}", target.origin_key, target.request_id).as_bytes())
    );
    let equivalent = || {
        let mut previous: Receipt =
            serde_json::from_slice(&read(&receipts, &original_key).unwrap()).unwrap();
        let mut next = target.clone();
        next.request_id = op();
        previous.intent.request_id = next.request_id.clone();
        previous.file_operation_generation = None;
        let key = format!(
            "{}.json",
            digest(format!("{}:{}", next.origin_key, next.request_id).as_bytes())
        );
        save(&receipts, &key, &previous, true).unwrap();
        next
    };
    let next = equivalent();
    assert!(is_applied(call(
        root.path(),
        &next,
        FileOperation::Reconcile {
            operation_id: operation_id.clone()
        }
    )));
    std::fs::write(Path::new(&cwd).join("selected.md"), "agent changed it").unwrap();
    assert!(is_applied(call(
        root.path(),
        &next,
        write(&operation_id, "selected.md", Expected::Absent, b"initial")
    )));
    assert_eq!(
        std::fs::read_to_string(Path::new(&cwd).join("selected.md")).unwrap(),
        "agent changed it"
    );
    std::fs::rename(
        root.path().join("remote-file-operations"),
        root.path().join("lost-ledger"),
    )
    .unwrap();
    let third = equivalent();
    assert!(matches!(
        call(
            root.path(),
            &third,
            FileOperation::Reconcile { operation_id }
        ),
        FileReply::Effect {
            outcome: EffectOutcome::Unknown { .. }
        }
    ));
    assert!(
        !root.path().join("remote-file-operations").exists(),
        "new preparation evidence must not recreate an established lost namespace"
    );
}

#[test]
fn hydration_nested_parent_creation_preserves_competing_file_and_symlink() {
    let (root, target, cwd, _) = workspace();
    let path = "context/missing/deeper/current.md";
    // File effects deliberately do not synthesize directories; the engine must
    // first acknowledge the separate, contained directory operation.
    assert!(matches!(
        call(
            root.path(),
            &target,
            write(&op(), path, Expected::Absent, b"saved context")
        ),
        FileReply::Effect {
            outcome: EffectOutcome::Conflict {
                observation: Observation::Absent
            }
        }
    ));
    assert!(is_applied(call(
        root.path(),
        &target,
        FileOperation::EnsureDirectories {
            operation_id: op(),
            path: "context/missing/deeper".into()
        }
    )));
    assert!(is_applied(call(
        root.path(),
        &target,
        write(&op(), path, Expected::Absent, b"saved context")
    )));
    assert_eq!(
        std::fs::read(Path::new(&cwd).join(path)).unwrap(),
        b"saved context"
    );
    std::fs::write(Path::new(&cwd).join("file-parent"), b"preserve file").unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), Path::new(&cwd).join("link-parent")).unwrap();
    for parent in ["file-parent", "link-parent"] {
        assert!(matches!(
            call(
                root.path(),
                &target,
                FileOperation::EnsureDirectories {
                    operation_id: op(),
                    path: format!("{parent}/nested")
                }
            ),
            FileReply::Effect {
                outcome: EffectOutcome::Conflict { .. }
            }
        ));
    }
    assert_eq!(
        std::fs::read(Path::new(&cwd).join("file-parent")).unwrap(),
        b"preserve file"
    );
    assert!(
        std::fs::symlink_metadata(Path::new(&cwd).join("link-parent"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!outside.path().join("nested").exists());
}

fn git_in(cwd: &str, args: &[&str]) {
    assert!(
        strip_attribution_env(&mut std::process::Command::new("git"))
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
            ])
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap()
            .success()
    );
}
fn changes(
    root: &Path,
    target: &FileTarget,
    base_ref: Option<&str>,
) -> (Vec<ChangedFileView>, String) {
    match call(
        root,
        target,
        FileOperation::Changes {
            base_ref: base_ref.map(str::to_string),
        },
    ) {
        FileReply::Changes { files, stamp, .. } => (
            files
                .into_iter()
                .map(|f| ChangedFileView {
                    path: f.path,
                    status: f.status,
                    old_path: f.old_path,
                    counts: f.added.zip(f.removed),
                    mode_changed: f.mode_changed,
                })
                .collect(),
            stamp,
        ),
        other => panic!("unexpected reply {other:?}"),
    }
}
#[derive(Debug, PartialEq)]
struct ChangedFileView {
    path: String,
    status: codesense_pty_core::remote_files::ChangeStatus,
    old_path: Option<String>,
    counts: Option<(u32, u32)>,
    mode_changed: bool,
}
fn view(
    path: &str,
    status: codesense_pty_core::remote_files::ChangeStatus,
    old_path: Option<&str>,
    counts: Option<(u32, u32)>,
    mode_changed: bool,
) -> ChangedFileView {
    ChangedFileView {
        path: path.into(),
        status,
        old_path: old_path.map(str::to_string),
        counts,
        mode_changed,
    }
}
fn redline(
    root: &Path,
    target: &FileTarget,
    path: &str,
    old_path: Option<&str>,
    base_ref: Option<&str>,
) -> (Option<Vec<u8>>, Option<Vec<u8>>, bool) {
    match call(
        root,
        target,
        FileOperation::Redline {
            path: path.into(),
            old_path: old_path.map(str::to_string),
            base_ref: base_ref.map(str::to_string),
        },
    ) {
        FileReply::Redline {
            before,
            after,
            opaque,
        } => (before, after, opaque),
        other => panic!("unexpected reply {other:?}"),
    }
}

/// A worktree with committed branch work plus every uncommitted change shape.
fn diff_workspace() -> (tempfile::TempDir, FileTarget, String) {
    let (root, target, cwd, _) = workspace();
    let long_doc: String = (0..40).map(|i| format!("line {i} of the doc\n")).collect();
    std::fs::write(format!("{cwd}/a.txt"), "one\n").unwrap();
    std::fs::write(format!("{cwd}/b.txt"), "keep\n").unwrap();
    std::fs::write(format!("{cwd}/old.md"), &long_doc).unwrap();
    std::fs::write(format!("{cwd}/run.sh"), "echo hi\n").unwrap();
    std::fs::write(format!("{cwd}/img.bin"), [0u8, 1, 2, 0, 9]).unwrap();
    git_in(&cwd, &["add", "-A"]);
    git_in(&cwd, &["commit", "-q", "-m", "agent work"]);
    std::fs::write(format!("{cwd}/a.txt"), "one\ntwo\n").unwrap();
    std::fs::remove_file(format!("{cwd}/b.txt")).unwrap();
    git_in(&cwd, &["mv", "old.md", "new.md"]);
    std::fs::write(format!("{cwd}/new.md"), format!("{long_doc}one more\n")).unwrap();
    git_in(&cwd, &["update-index", "--chmod=+x", "run.sh"]);
    std::fs::set_permissions(
        format!("{cwd}/run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(format!("{cwd}/u.txt"), "x\ny").unwrap();
    (root, target, cwd)
}

#[test]
fn diff_against_fork_point_includes_committed_branch_work() {
    use codesense_pty_core::remote_files::ChangeStatus::*;
    let (root, target, _cwd) = diff_workspace();
    // The prepared clone has no `main`: only the preparation commit can anchor
    // the fork point, so committed work must still appear (not just dirty files).
    let (files, _) = changes(root.path(), &target, None);
    assert_eq!(
        files,
        vec![
            view("a.txt", Added, None, Some((2, 0)), false),
            view("img.bin", Added, None, None, false),
            view("new.md", Added, None, Some((41, 0)), false),
            view("run.sh", Added, None, Some((1, 0)), false),
            view("u.txt", Added, None, Some((2, 0)), false),
        ]
    );
}

#[test]
fn diff_against_head_classifies_every_uncommitted_shape() {
    use codesense_pty_core::remote_files::ChangeStatus::*;
    let (root, target, _cwd) = diff_workspace();
    let (files, _) = changes(root.path(), &target, Some("HEAD"));
    assert_eq!(
        files,
        vec![
            view("a.txt", Modified, None, Some((1, 0)), false),
            view("b.txt", Deleted, None, Some((0, 1)), false),
            view("new.md", Modified, Some("old.md"), Some((1, 0)), false),
            view("run.sh", Modified, None, Some((0, 0)), true),
            view("u.txt", Added, None, Some((2, 0)), false),
        ]
    );
}

#[test]
fn redline_reads_both_sides_against_the_same_baseline() {
    let (root, target, _cwd) = diff_workspace();
    let r = root.path();
    assert_eq!(
        redline(r, &target, "a.txt", None, Some("HEAD")),
        (Some(b"one\n".to_vec()), Some(b"one\ntwo\n".to_vec()), false)
    );
    assert_eq!(
        redline(r, &target, "a.txt", None, None),
        (None, Some(b"one\ntwo\n".to_vec()), false),
        "added on the branch: no baseline side"
    );
    let (before, after, _) = redline(r, &target, "b.txt", None, Some("HEAD"));
    assert_eq!((before.as_deref(), after), (Some(&b"keep\n"[..]), None));
    let (before, after, _) = redline(r, &target, "new.md", Some("old.md"), Some("HEAD"));
    assert!(before.unwrap().starts_with(b"line 0 of the doc\n"));
    assert!(after.unwrap().ends_with(b"one more\n"));
    let (before, _, _) = redline(r, &target, "img.bin", None, Some("HEAD"));
    assert_eq!(
        before,
        Some(vec![0u8, 1, 2, 0, 9]),
        "binary bytes cross intact"
    );
}

#[test]
fn diff_stamp_moves_on_a_same_line_count_edit() {
    let (root, target, cwd) = diff_workspace();
    let (_, first) = changes(root.path(), &target, None);
    let (_, again) = changes(root.path(), &target, None);
    assert_eq!(first, again, "a quiet worktree keeps its stamp");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(format!("{cwd}/a.txt"), "one\ntwX\n").unwrap();
    let (_, edited) = changes(root.path(), &target, None);
    assert_ne!(first, edited);
}

#[test]
fn diff_refuses_escaping_paths_and_option_shaped_refs() {
    let (root, target, _cwd) = diff_workspace();
    for path in ["../outside", ".git/config", "/etc/passwd"] {
        assert!(handle(
            root.path(),
            41,
            FileRequest {
                target: target.clone(),
                operation: FileOperation::Redline {
                    path: path.into(),
                    old_path: None,
                    base_ref: None
                },
            },
            || true
        )
        .is_err());
    }
    // An option-shaped ref is never handed to Git; the ladder still answers.
    let probe = tempfile::tempdir().unwrap();
    let pwned = probe.path().join("pwned");
    let (files, _) = changes(
        root.path(),
        &target,
        Some(&format!("--output={}", pwned.display())),
    );
    assert!(!files.is_empty());
    assert!(!pwned.exists());
}

#[test]
fn diff_reads_never_follow_a_symlink_out_of_the_worktree() {
    let (root, target, cwd, _) = workspace();
    let cwd = Path::new(&cwd);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "outside secret\n").unwrap();
    symlink(outside.path(), cwd.join("linked")).unwrap();
    symlink(outside.path().join("secret.txt"), cwd.join("leaf.txt")).unwrap();
    let refused = |operation| {
        handle(
            root.path(),
            41,
            FileRequest {
                target: target.clone(),
                operation,
            },
            || true,
        )
    };
    for path in ["linked/secret.txt", "leaf.txt"] {
        match refused(FileOperation::Redline {
            path: path.into(),
            old_path: None,
            base_ref: Some("HEAD".into()),
        }) {
            Err(_) => {}
            Ok(FileReply::Redline { after, .. }) => assert!(
                !after.unwrap_or_default().starts_with(b"outside"),
                "{path} must not read through the link"
            ),
            Ok(other) => panic!("unexpected reply {other:?}"),
        }
    }
    // The untracked link is listed as a change but never read through.
    let (files, _) = changes(root.path(), &target, Some("HEAD"));
    let leaf = files.iter().find(|f| f.path == "leaf.txt").expect("listed");
    assert_eq!(leaf.counts, None);
}

/// Commit `file` (its name as content) on the checked-out branch.
fn commit_file(cwd: &str, file: &str) {
    std::fs::write(format!("{cwd}/{file}"), format!("{file}\n")).unwrap();
    git_in(cwd, &["add", "-A"]);
    git_in(cwd, &["commit", "-q", "-m", file]);
}

/// Upstream commits `upstream.txt` on top of `from` and publishes it as `as_ref`,
/// leaving the workspace branch checked out and untouched.
fn upstream_moves(cwd: &str, from: &str, as_ref: &str) {
    git_in(cwd, &["checkout", "-q", "--detach", from]);
    commit_file(cwd, "upstream.txt");
    git_in(cwd, &["update-ref", as_ref, "HEAD"]);
    git_in(cwd, &["checkout", "-q", "-"]);
}

/// A workspace whose branch is named `main` (a clone whose own HEAD names
/// another branch lets a worktree take `main`).
fn main_named_fixture() -> (tempfile::TempDir, PrepareWorkspace) {
    let (root, first) = fixture();
    assert_eq!(prepared(root.path(), first.clone()).status, "prepared");
    git_at(
        Path::new(&first.repo_path),
        &["symbolic-ref", "HEAD", "refs/heads/master"],
    );
    let mut request = first.clone();
    request.request_id = "00000000-0000-4000-8000-000000000002".into();
    request.worktree_path = format!("{}-main", first.worktree_path);
    request.branch = "main".into();
    (root, request)
}

/// The baseline rule (module docs of `diff.rs`), one row per case. Every row
/// starts from a freshly prepared workspace whose prep fork is the
/// preparation commit; `setup` shapes the history, then the default (or
/// named) diff must list exactly `expect`, each an added one-line file.
#[test]
fn baseline_rule_cases() {
    type Setup = fn(&str);
    type Fixture = fn() -> (tempfile::TempDir, PrepareWorkspace);
    type Case = (
        &'static str,
        Fixture,
        Setup,
        Option<&'static str>,
        &'static [&'static str],
    );
    let cases: &[Case] = &[
        (
            "no main on the host: the prep fork anchors committed work",
            fixture,
            |cwd| commit_file(cwd, "mine.txt"),
            None,
            &["mine.txt"],
        ),
        (
            "a stale origin/main older than the prep fork loses",
            fixture_with_history,
            |cwd| {
                commit_file(cwd, "mine.txt");
                git_in(cwd, &["update-ref", "refs/remotes/origin/main", "HEAD~2"]);
            },
            None,
            &["mine.txt"],
        ),
        (
            "origin/main moved on but not merged: forks at the prep commit",
            fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                upstream_moves(cwd, "HEAD~1", "refs/remotes/origin/main");
            },
            None,
            &["mine.txt"],
        ),
        (
            "main merged in: the newer ladder fork wins",
            fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                upstream_moves(cwd, "HEAD~1", "refs/remotes/origin/main");
                git_in(cwd, &["merge", "-q", "--no-edit", "origin/main"]);
            },
            None,
            &["mine.txt"],
        ),
        (
            "rebased onto main: the newer ladder fork wins",
            fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                upstream_moves(cwd, "HEAD~1", "refs/remotes/origin/main");
                git_in(cwd, &["rebase", "-q", "origin/main"]);
            },
            None,
            &["mine.txt"],
        ),
        (
            "fast-forwarded onto main with no work: nothing listed",
            fixture,
            |cwd| {
                upstream_moves(cwd, "HEAD", "refs/remotes/origin/main");
                git_in(cwd, &["merge", "-q", "--ff-only", "origin/main"]);
            },
            None,
            &[],
        ),
        (
            "fast-forwarded onto main, then committed: only that commit",
            fixture,
            |cwd| {
                upstream_moves(cwd, "HEAD", "refs/remotes/origin/main");
                git_in(cwd, &["merge", "-q", "--ff-only", "origin/main"]);
                commit_file(cwd, "mine.txt");
            },
            None,
            &["mine.txt"],
        ),
        (
            "branch named main: the main rungs are the branch, so skipped",
            main_named_fixture,
            |cwd| commit_file(cwd, "mine.txt"),
            None,
            &["mine.txt"],
        ),
        (
            "branch named main, pushed to origin/main: still skipped",
            main_named_fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                git_in(cwd, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
                git_in(
                    cwd,
                    &[
                        "symbolic-ref",
                        "refs/remotes/origin/HEAD",
                        "refs/remotes/origin/main",
                    ],
                );
                git_in(cwd, &["branch", "-q", "--set-upstream-to", "origin/main"]);
            },
            None,
            &["mine.txt"],
        ),
        (
            "branch named main pulls upstream: ambiguous, so shows more",
            main_named_fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                upstream_moves(cwd, "HEAD~1", "refs/remotes/origin/main");
                git_in(cwd, &["merge", "-q", "--no-edit", "origin/main"]);
            },
            None,
            &["mine.txt", "upstream.txt"],
        ),
        (
            "pushed with -u under its own name: that rung is skipped",
            fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                git_in(
                    cwd,
                    &["update-ref", "refs/remotes/origin/work-branch", "HEAD"],
                );
                git_in(
                    cwd,
                    &["branch", "-q", "--set-upstream-to", "origin/work-branch"],
                );
                commit_file(cwd, "later.txt");
            },
            None,
            &["later.txt", "mine.txt"],
        ),
        (
            "upstream under another name at HEAD: its work stays",
            fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                git_in(
                    cwd,
                    &["update-ref", "refs/remotes/origin/review/work", "HEAD"],
                );
                git_in(
                    cwd,
                    &["branch", "-q", "--set-upstream-to", "origin/review/work"],
                );
            },
            None,
            &["mine.txt"],
        ),
        (
            "upstream under another name, committed on top: all work stays",
            fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                git_in(
                    cwd,
                    &["update-ref", "refs/remotes/origin/review/work", "HEAD"],
                );
                git_in(
                    cwd,
                    &["branch", "-q", "--set-upstream-to", "origin/review/work"],
                );
                commit_file(cwd, "later.txt");
            },
            None,
            &["later.txt", "mine.txt"],
        ),
        (
            "a tracked develop default merged in: the upstream fork wins",
            fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                upstream_moves(cwd, "HEAD~1", "refs/remotes/origin/develop");
                git_in(
                    cwd,
                    &["branch", "-q", "--set-upstream-to", "origin/develop"],
                );
                git_in(cwd, &["merge", "-q", "--no-edit", "origin/develop"]);
            },
            None,
            &["mine.txt"],
        ),
        (
            "rebased onto a tracked develop: ambiguous upstream, so shows more",
            fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                upstream_moves(cwd, "HEAD~1", "refs/remotes/origin/develop");
                git_in(
                    cwd,
                    &["branch", "-q", "--set-upstream-to", "origin/develop"],
                );
                git_in(cwd, &["rebase", "-q", "origin/develop"]);
            },
            None,
            &["mine.txt", "upstream.txt"],
        ),
        (
            "a ref the caller names wins over the prep fork",
            fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                git_in(cwd, &["tag", "reviewed"]);
                commit_file(cwd, "later.txt");
            },
            Some("reviewed"),
            &["later.txt"],
        ),
        (
            "a named ref that does not resolve is skipped",
            fixture,
            |cwd| commit_file(cwd, "mine.txt"),
            Some("no-such-ref"),
            &["mine.txt"],
        ),
        (
            "base ref HEAD: uncommitted work only",
            fixture,
            |cwd| {
                commit_file(cwd, "mine.txt");
                std::fs::write(format!("{cwd}/dirty.txt"), "dirty.txt\n").unwrap();
            },
            Some("HEAD"),
            &["dirty.txt"],
        ),
    ];
    use codesense_pty_core::remote_files::ChangeStatus::Added;
    for (name, make, setup, base_ref, expect) in cases {
        let (root, target, cwd, _) = workspace_from(make());
        setup(&cwd);
        let (files, _) = changes(root.path(), &target, *base_ref);
        let want: Vec<_> = expect
            .iter()
            .map(|path| view(path, Added, None, Some((1, 0)), false))
            .collect();
        assert_eq!(files, want, "case: {name}");
    }
}

#[test]
fn diff_lists_a_path_untracked_since_the_baseline_once() {
    use codesense_pty_core::remote_files::ChangeStatus::*;
    let (root, target, cwd, _) = workspace();
    std::fs::write(format!("{cwd}/dropped.txt"), "same\n").unwrap();
    git_in(&cwd, &["add", "-A"]);
    git_in(&cwd, &["commit", "-q", "-m", "track it"]);
    git_in(&cwd, &["rm", "-q", "--cached", "dropped.txt"]);
    let (files, _) = changes(root.path(), &target, Some("HEAD"));
    assert_eq!(
        files,
        vec![view("dropped.txt", Deleted, None, Some((0, 1)), false)]
    );
}

#[test]
fn redline_marks_link_sides_opaque_and_a_directory_at_a_deleted_name_absent() {
    let (root, target, cwd, _) = workspace();
    symlink("target.txt", format!("{cwd}/link")).unwrap();
    std::fs::write(format!("{cwd}/plain.txt"), "plain\n").unwrap();
    std::fs::write(format!("{cwd}/was_file"), "file\n").unwrap();
    git_in(&cwd, &["add", "-A"]);
    git_in(&cwd, &["commit", "-q", "-m", "shapes"]);
    // A tracked link replaced by a file: its baseline side is no content.
    std::fs::remove_file(format!("{cwd}/link")).unwrap();
    std::fs::write(format!("{cwd}/link"), "now a file\n").unwrap();
    let (_, after, opaque) = redline(root.path(), &target, "link", None, Some("HEAD"));
    assert!(opaque, "a link's target is not file content");
    assert_eq!(after.as_deref(), Some(&b"now a file\n"[..]));
    // A tracked file replaced by a link: its working side is no content.
    std::fs::remove_file(format!("{cwd}/plain.txt")).unwrap();
    symlink("elsewhere", format!("{cwd}/plain.txt")).unwrap();
    let (before, _, opaque) = redline(root.path(), &target, "plain.txt", None, Some("HEAD"));
    assert!(opaque);
    assert_eq!(before.as_deref(), Some(&b"plain\n"[..]));
    // A deleted file with a directory now at its name reads as deleted.
    std::fs::remove_file(format!("{cwd}/was_file")).unwrap();
    std::fs::create_dir(format!("{cwd}/was_file")).unwrap();
    std::fs::write(format!("{cwd}/was_file/inner.txt"), "inner\n").unwrap();
    assert_eq!(
        redline(root.path(), &target, "was_file", None, Some("HEAD")),
        (Some(b"file\n".to_vec()), None, false)
    );
}

#[test]
fn redline_of_a_changed_submodule_is_opaque_not_a_deletion() {
    let (root, target, cwd, _) = workspace();
    let sub = tempfile::tempdir().unwrap();
    let sub_path = sub.path().to_str().unwrap().to_string();
    git_in(&sub_path, &["init", "-q", "-b", "main"]);
    git_in(&sub_path, &["commit", "-q", "--allow-empty", "-m", "one"]);
    git_in(
        &cwd,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &sub_path,
            "vendored",
        ],
    );
    git_in(&cwd, &["commit", "-q", "-m", "add submodule"]);
    git_in(
        &format!("{cwd}/vendored"),
        &["commit", "-q", "--allow-empty", "-m", "two"],
    );
    let (before, after, opaque) = redline(root.path(), &target, "vendored", None, Some("HEAD"));
    assert!(opaque);
    assert!(
        before.is_some() && after.is_some(),
        "a modified submodule, not a deletion"
    );
}

#[test]
fn redline_of_a_directory_reads_by_what_the_baseline_held() {
    let (root, target, cwd, _) = workspace();
    symlink("target.txt", format!("{cwd}/was_link")).unwrap();
    git_in(&cwd, &["add", "-A"]);
    git_in(&cwd, &["commit", "-q", "-m", "a link"]);
    // A tracked link with a directory now at its name: deleted, as Changes says.
    std::fs::remove_file(format!("{cwd}/was_link")).unwrap();
    std::fs::create_dir(format!("{cwd}/was_link")).unwrap();
    std::fs::write(format!("{cwd}/was_link/inner.txt"), "inner\n").unwrap();
    let (before, after, _) = redline(root.path(), &target, "was_link", None, Some("HEAD"));
    assert!(before.is_some() && after.is_none());
    // No baseline entry at all (a new nested repo): the entry itself, opaque.
    std::fs::create_dir(format!("{cwd}/nested")).unwrap();
    assert_eq!(
        redline(root.path(), &target, "nested", None, Some("HEAD")),
        (None, Some(Vec::new()), true)
    );
}

/// The public Changes path is bounded by the frame budget (here shrunk).
#[test]
fn changes_refuses_an_oversized_reply() {
    use crate::remote_workspace::diff::TEST_BUDGET;
    let (root, target, _cwd) = diff_workspace();
    TEST_BUDGET.with(|b| b.set(Some(64)));
    let refused = handle(
        root.path(),
        41,
        FileRequest {
            target,
            operation: FileOperation::Changes { base_ref: None },
        },
        || true,
    );
    TEST_BUDGET.with(|b| b.set(None));
    assert!(refused.unwrap_err().contains("exceeds"));
}

/// A Git call killed mid-run on the public Changes path (a clean filter that
/// hangs `git diff` after the baseline resolved) reads as a retryable timeout
/// under live authority and as access loss once the authority retired, never
/// as "reconcile": proof that `changes` re-words the kill.
#[test]
fn changes_rewords_a_git_call_killed_mid_run() {
    use crate::remote_workspace::diff::TEST_DEADLINE;
    use std::time::{Duration, Instant};
    let (root, target, cwd) = diff_workspace();
    // The filter marks that it ran: the kill then landed mid-call, never in
    // the pre-call `out_of_time` check.
    let marker = format!("{cwd}/../filter-ran");
    git_in(
        &cwd,
        &[
            "config",
            "filter.slow.clean",
            &format!("touch '{marker}'; sleep 30; cat"),
        ],
    );
    std::fs::write(format!("{cwd}/.gitattributes"), "a.txt filter=slow\n").unwrap();
    let run = |alive: &dyn Fn() -> bool| {
        let _ = std::fs::remove_file(&marker);
        let err = handle(
            root.path(),
            41,
            FileRequest {
                target: target.clone(),
                operation: FileOperation::Changes { base_ref: None },
            },
            alive,
        )
        .unwrap_err();
        assert!(
            std::path::Path::new(&marker).exists(),
            "the kill must land inside `git diff`: {err}"
        );
        err
    };
    // The deadline lapses while the filter holds `git diff`.
    TEST_DEADLINE.with(|d| d.set(Some(Duration::from_secs(3))));
    let started = Instant::now();
    let timed_out = run(&|| true);
    TEST_DEADLINE.with(|d| d.set(None));
    assert_eq!(timed_out, crate::remote_workspace::GIT_TIMED_OUT);
    assert!(started.elapsed() < Duration::from_secs(20));
    // The authority retires while the filter holds `git diff`.
    let started = Instant::now();
    let retired = run(&|| started.elapsed() < Duration::from_secs(3));
    assert_eq!(retired, crate::remote_workspace::AUTHORITY_RETIRED);
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn diff_lists_a_rename_source_with_a_new_file_at_its_name_once() {
    use codesense_pty_core::remote_files::ChangeStatus::*;
    let (root, target, cwd, _) = workspace();
    let doc: String = (0..20).map(|i| format!("line {i}\n")).collect();
    std::fs::write(format!("{cwd}/a.txt"), &doc).unwrap();
    git_in(&cwd, &["add", "-A"]);
    git_in(&cwd, &["commit", "-q", "-m", "track it"]);
    git_in(&cwd, &["mv", "a.txt", "b.txt"]);
    std::fs::write(format!("{cwd}/a.txt"), "").unwrap();
    let (files, _) = changes(root.path(), &target, Some("HEAD"));
    assert_eq!(
        files,
        vec![view("b.txt", Modified, Some("a.txt"), Some((0, 0)), false)]
    );
}

#[test]
fn redline_reads_a_deleted_file_under_a_replaced_parent_as_absent() {
    let (root, target, cwd, _) = workspace();
    std::fs::create_dir(format!("{cwd}/dir")).unwrap();
    std::fs::write(format!("{cwd}/dir/gone.txt"), "was here\n").unwrap();
    git_in(&cwd, &["add", "-A"]);
    git_in(&cwd, &["commit", "-q", "-m", "track it"]);
    std::fs::remove_dir_all(format!("{cwd}/dir")).unwrap();
    std::fs::write(format!("{cwd}/dir"), "now a file\n").unwrap();
    assert_eq!(
        redline(root.path(), &target, "dir/gone.txt", None, Some("HEAD")),
        (Some(b"was here\n".to_vec()), None, false)
    );
    std::fs::remove_file(format!("{cwd}/dir")).unwrap();
    symlink("/tmp", format!("{cwd}/dir")).unwrap();
    assert_eq!(
        redline(root.path(), &target, "dir/gone.txt", None, Some("HEAD")),
        (Some(b"was here\n".to_vec()), None, false)
    );
}

/// A remove whose target is already gone is `Ok(AlreadyAbsent)`,
/// whatever pre-hash it carried.
#[test]
fn remove_of_an_already_absent_file_is_applied() {
    let (root, target, _, _) = workspace();
    let reply = call(
        root.path(),
        &target,
        FileOperation::Apply {
            operation_id: op(),
            path: "gone/already.md".into(),
            expected: Expected::Hash {
                hash: *blake3::hash(b"what the diff saw").as_bytes(),
            },
            effect: Effect::Remove,
        },
    );
    assert!(
        matches!(
            reply,
            FileReply::Effect {
                outcome: EffectOutcome::Applied {
                    observation: Observation::Absent
                }
            }
        ),
        "{reply:?}"
    );
}

/// A pre-hash mismatch where the file already holds the target
/// bytes (two launches raced) is success, not a conflict.
#[test]
fn write_whose_target_already_matches_is_applied_despite_pre_hash_mismatch() {
    let (root, target, cwd, _) = workspace();
    std::fs::write(Path::new(&cwd).join("raced.md"), b"same target").unwrap();
    std::fs::set_permissions(
        Path::new(&cwd).join("raced.md"),
        std::os::unix::fs::PermissionsExt::from_mode(0o640),
    )
    .unwrap();
    let reply = call(
        root.path(),
        &target,
        write(&op(), "raced.md", Expected::Absent, b"same target"),
    );
    assert!(is_applied(reply.clone()), "{reply:?}");
    // A different target under the same mismatch stays a conflict.
    let reply = call(
        root.path(),
        &target,
        write(&op(), "raced.md", Expected::Absent, b"other target"),
    );
    assert!(matches!(
        reply,
        FileReply::Effect {
            outcome: EffectOutcome::Conflict { .. }
        }
    ));
    assert_eq!(
        std::fs::read(Path::new(&cwd).join("raced.md")).unwrap(),
        b"same target"
    );
}

#[test]
fn editor_list_is_one_level_dirs_first_and_ignores_heavy_siblings() {
    let (root, target, cwd, _) = workspace();
    let cwd = Path::new(&cwd);
    std::fs::create_dir_all(cwd.join("src/nested")).unwrap();
    std::fs::write(cwd.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(cwd.join("src/nested/deep.rs"), "").unwrap();
    std::fs::write(cwd.join("README.md"), "hello").unwrap();
    // A dependency tree far larger than an Enumerate limit must not matter to a
    // one-level listing of the root.
    std::fs::create_dir(cwd.join("node_modules")).unwrap();
    for n in 0..50 {
        std::fs::create_dir(cwd.join(format!("node_modules/pkg{n}"))).unwrap();
    }
    let FileReply::Listing { entries } =
        call(root.path(), &target, FileOperation::List { path: None })
    else {
        panic!("expected a listing");
    };
    let names: Vec<_> = entries
        .iter()
        .map(|e| (e.name.as_str(), e.is_dir))
        .collect();
    assert_eq!(
        names,
        vec![("node_modules", true), ("src", true), ("README.md", false),]
    );
    let readme = entries
        .iter()
        .find(|e| e.name == "README.md")
        .expect("README listed");
    assert_eq!(readme.size, Some(5));
    assert_eq!(entries[0].size, None);

    let FileReply::Listing { entries } = call(
        root.path(),
        &target,
        FileOperation::List {
            path: Some("src".into()),
        },
    ) else {
        panic!("expected a listing");
    };
    let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["nested", "main.rs"]);

    for escape in ["../outside", ".git", "src/../.."] {
        assert!(
            handle(
                root.path(),
                41,
                FileRequest {
                    target: target.clone(),
                    operation: FileOperation::List {
                        path: Some(escape.into())
                    }
                },
                || true
            )
            .is_err(),
            "{escape} must be refused"
        );
    }
    assert!(handle(
        root.path(),
        41,
        FileRequest {
            target,
            operation: FileOperation::List {
                path: Some("missing".into())
            }
        },
        || true
    )
    .is_err());
}

#[test]
fn editor_read_head_returns_committed_bytes_or_none() {
    let (root, target, cwd, _) = workspace();
    let cwd = Path::new(&cwd);
    std::fs::create_dir(cwd.join("src")).unwrap();
    std::fs::write(cwd.join("src/lib.rs"), "committed\n").unwrap();
    git_at(cwd, &["add", "src/lib.rs"]);
    git_at(cwd, &["commit", "-m", "add lib"]);
    // The working copy moves on; HEAD keeps answering the committed bytes.
    std::fs::write(cwd.join("src/lib.rs"), "edited\n").unwrap();
    std::fs::write(cwd.join("src/new.rs"), "untracked\n").unwrap();
    let head = |path: &str| {
        call(
            root.path(),
            &target,
            FileOperation::ReadHead { path: path.into() },
        )
    };
    assert_eq!(
        head("src/lib.rs"),
        FileReply::Head {
            bytes: Some(b"committed\n".to_vec())
        }
    );
    assert_eq!(head("src/new.rs"), FileReply::Head { bytes: None });
    // A directory is a tree entry, not a file baseline.
    assert_eq!(head("src"), FileReply::Head { bytes: None });
    assert!(handle(
        root.path(),
        41,
        FileRequest {
            target,
            operation: FileOperation::ReadHead {
                path: "../escape".into()
            }
        },
        || true
    )
    .is_err());
}

#[test]
fn editor_read_head_of_a_blob_past_the_reply_cap_has_no_baseline() {
    let (root, target, cwd, _) = workspace();
    let cwd = Path::new(&cwd);
    std::fs::write(cwd.join("big.bin"), vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();
    git_at(cwd, &["add", "big.bin"]);
    git_at(cwd, &["commit", "-m", "big"]);
    assert_eq!(
        call(
            root.path(),
            &target,
            FileOperation::ReadHead {
                path: "big.bin".into()
            },
        ),
        FileReply::Head { bytes: None }
    );
}

#[test]
fn editor_list_files_offers_tracked_and_untracked_but_not_ignored() {
    let (root, target, cwd, _) = workspace();
    let cwd = Path::new(&cwd);
    std::fs::write(cwd.join(".gitignore"), "dist/\n").unwrap();
    std::fs::write(cwd.join("tracked.rs"), "").unwrap();
    git_at(cwd, &["add", ".gitignore", "tracked.rs"]);
    git_at(cwd, &["commit", "-m", "files"]);
    std::fs::write(cwd.join("untracked.rs"), "").unwrap();
    std::fs::create_dir(cwd.join("dist")).unwrap();
    std::fs::write(cwd.join("dist/bundle.js"), "").unwrap();
    assert_eq!(
        call(
            root.path(),
            &target,
            FileOperation::ListFiles { limit: 100 }
        ),
        FileReply::FileList {
            paths: vec![
                ".gitignore".into(),
                "tracked.rs".into(),
                "untracked.rs".into()
            ],
            truncated: false,
        }
    );
    assert_eq!(
        call(root.path(), &target, FileOperation::ListFiles { limit: 2 }),
        FileReply::FileList {
            paths: vec![".gitignore".into(), "tracked.rs".into()],
            truncated: true,
        }
    );
    // A tracked file deleted from the working tree isn't offered.
    std::fs::remove_file(cwd.join("tracked.rs")).unwrap();
    assert_eq!(
        call(
            root.path(),
            &target,
            FileOperation::ListFiles { limit: 100 }
        ),
        FileReply::FileList {
            paths: vec![".gitignore".into(), "untracked.rs".into()],
            truncated: false,
        }
    );
}

#[test]
fn editor_reads_never_follow_a_symlink_out_of_the_worktree() {
    let (root, target, cwd, _) = workspace();
    let cwd = Path::new(&cwd);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "outside secret\n").unwrap();
    symlink(outside.path(), cwd.join("linked")).unwrap();
    symlink(outside.path().join("secret.txt"), cwd.join("leaf.txt")).unwrap();
    let refused = |operation| {
        handle(
            root.path(),
            41,
            FileRequest {
                target: target.clone(),
                operation,
            },
            || true,
        )
    };
    // A symlinked directory is never descended, for a listing or through a path.
    assert!(refused(FileOperation::List {
        path: Some("linked".into())
    })
    .is_err());
    // Listing the root shows the links as entries, not their targets' contents.
    let FileReply::Listing { entries } =
        call(root.path(), &target, FileOperation::List { path: None })
    else {
        panic!("expected a listing");
    };
    let linked = entries.iter().find(|e| e.name == "linked").unwrap();
    assert!(!linked.is_dir);
}

#[test]
fn editor_read_head_on_an_unborn_branch_has_no_baseline() {
    let (root, target, cwd, _) = workspace();
    // The checked-out branch loses its commit: HEAD still names it, unborn.
    git_at(
        Path::new(&cwd),
        &["update-ref", "-d", "refs/heads/work-branch"],
    );
    std::fs::write(format!("{cwd}/new.rs"), "x\n").unwrap();
    assert_eq!(
        call(
            root.path(),
            &target,
            FileOperation::ReadHead {
                path: "new.rs".into()
            }
        ),
        FileReply::Head { bytes: None }
    );
}
