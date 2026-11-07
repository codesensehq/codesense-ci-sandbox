// SPDX-License-Identifier: MIT
//! SW-XS-7 acceptance tests for the one preparation engine.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use super::*;
use crate::commands::worktree::EventSink;
use crate::content_engine::destination::*;
use codesense_pty_core::context_batch::{
    MemberOutcome, MemberResult, OperationKind, OperationResult, TransferPhase,
};
use codesense_pty_core::remote_files::{EffectOutcome, Observation as WireObservation};
use codesense_pty_core::remote_manifest::{
    EntryKind, ManifestEntry, ManifestProgress, WorkspaceExclude,
};

fn principal() -> Principal {
    Principal::from(42u64)
}

/// Records every progress event as the app would emit it.
#[derive(Default, Clone)]
struct RecordingEventSink(Arc<Mutex<Vec<(String, serde_json::Value)>>>);
impl EventSink for RecordingEventSink {
    fn emit(&self, channel: &str, payload: serde_json::Value) {
        self.0.lock().unwrap().push((channel.into(), payload));
    }
}
impl RecordingEventSink {
    fn phases(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(channel, payload)| {
                assert_eq!(channel, PREPARE_PROGRESS_EVENT);
                payload["phase"].as_str().unwrap().to_string()
            })
            .collect()
    }
}

struct Fx {
    home: tempfile::TempDir,
    content: PathBuf,
    _dirs: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let dirs = tempfile::tempdir().unwrap();
        let content = dirs.path().join("content");
        let root = dirs.path().join("workspace");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        Self {
            home,
            content,
            _dirs: dirs,
            root,
        }
    }
    fn team(&self, path: &str, bytes: &[u8]) {
        write(&self.content, path, bytes);
    }
    fn want(&self, subtrees: &[&str]) -> WantSet {
        WantSet::from_subtrees(&self.content, subtrees, "team:o/t")
    }
    fn run_with(
        &self,
        root: &Path,
        want: &WantSet,
        legacy: Vec<(String, [u8; 32], String)>,
        prune: bool,
        sink: &dyn EventSink,
    ) -> Result<PrepareReport, String> {
        let legacy = move || Ok(legacy.clone());
        let emit = progress::to_sink(sink);
        let report = prepare(PrepareRequest {
            home: self.home.path(),
            principal: &principal(),
            destination: Destination::Local { root },
            want,
            origin: "step2:o/t/barb".into(),
            workspace: root.display().to_string(),
            agent: "Barb".into(),
            legacy: &legacy,
            prune,
            progress: &emit,
            validate: &|| Ok(()),
            explicit: Default::default(),
        })?;
        // As every caller does once its own post-checks pass.
        progress::finish(
            &emit,
            &report,
            &root.display().to_string(),
            "Barb",
            "team default",
        );
        Ok(report)
    }
    fn run(&self, want: &WantSet) -> PrepareReport {
        self.run_with(
            &self.root,
            want,
            Vec::new(),
            false,
            &crate::commands::worktree::NullEventSink,
        )
        .unwrap()
    }
    fn read(&self, path: &str) -> Vec<u8> {
        std::fs::read(self.root.join(path)).unwrap()
    }
}

fn write(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = crate::test_support::strip_attribution_env(&mut Command::new("git"))
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn git_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    write(root, "README.md", b"code\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "init"]);
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

// ---------------------------------------------------------------- local

/// AC4: a local workspace with nothing changed hashes 0 files (the managed
/// record is the stat cache) and writes nothing.
#[test]
fn local_unchanged_workspace_hashes_nothing_and_writes_nothing() {
    let fx = Fx::new();
    for i in 0..50 {
        fx.team(
            &format!(".claude/skills/s{i}/SKILL.md"),
            format!("skill {i}").as_bytes(),
        );
    }
    let want = fx.want(&[".claude/skills"]);
    let first = fx.run(&want);
    assert_eq!(first.status, PrepareStatus::Ready);
    assert_eq!(first.written.len(), 50);
    let second = fx.run(&want);
    assert_eq!(second.status, PrepareStatus::Ready);
    assert!(second.written.is_empty());
    assert_eq!(second.hashed, 0, "stat cache: no file read on either side");
    assert_eq!(second.unchanged, 50);
}

/// AC4 bench (non-gating): 2,000 files, nothing changed.
#[test]
#[ignore = "bench: SW-XS-7 AC4, run with --ignored --nocapture"]
fn bench_local_two_thousand_unchanged_files() {
    let fx = Fx::new();
    for i in 0..2_000 {
        fx.team(&format!("docs/d{}/f{i}.md", i % 20), &[b'x'; 2048]);
    }
    let want = fx.want(&["docs"]);
    fx.run(&want);
    let started = std::time::Instant::now();
    let report = fx.run(&want);
    let elapsed = started.elapsed();
    println!(
        "SW-XS-7 AC4: 2,000 unchanged files prepared in {elapsed:?}, hashed {}",
        report.hashed
    );
    assert_eq!(report.hashed, 0);
}

/// §11.1 + AC3: a stale unmodified managed file is replaced; one the user
/// modified is neither overwritten nor removed and is reported `kept
/// (modified)`; an unmanaged file is kept; nothing is removed on a launch.
#[test]
fn local_launch_replaces_stale_keeps_modified_and_never_removes() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a1");
    fx.team("docs/b.md", b"b1");
    fx.team("docs/gone.md", b"gone");
    write(&fx.root, "docs/user.md", b"user's own");
    fx.team("docs/user.md", b"team's");
    fx.run(&fx.want(&["docs"]));
    assert_eq!(fx.read("docs/user.md"), b"user's own");

    fx.team("docs/a.md", b"a2");
    fx.team("docs/b.md", b"b2");
    write(&fx.root, "docs/b.md", b"my edit");
    std::fs::remove_file(fx.content.join("docs/gone.md")).unwrap();
    let report = fx.run(&fx.want(&["docs"]));
    assert_eq!(report.status, PrepareStatus::Ready);
    assert_eq!(fx.read("docs/a.md"), b"a2");
    assert_eq!(fx.read("docs/b.md"), b"my edit");
    assert_eq!(report.kept_modified, vec!["docs/b.md"]);
    assert_eq!(report.kept_unmanaged, vec!["docs/user.md"]);
    assert!(
        fx.root.join("docs/gone.md").is_file(),
        "a launch never removes"
    );
    assert!(report
        .startup_note("team default")
        .contains("1 kept because you modified them: docs/b.md"));
}

/// AC2 (local): only an explicit prune removes, and only unmodified unwanted
/// managed files.
#[test]
fn local_prune_removes_only_unmodified_unwanted_managed_files() {
    let fx = Fx::new();
    fx.team("docs/x.md", b"x");
    fx.team("docs/y.md", b"y");
    fx.run(&fx.want(&["docs"]));
    write(&fx.root, "docs/y.md", b"edited");
    let empty = WantSet::default();
    let report = fx
        .run_with(
            &fx.root,
            &empty,
            Vec::new(),
            true,
            &crate::commands::worktree::NullEventSink,
        )
        .unwrap();
    assert_eq!(report.removed, vec!["docs/x.md"]);
    assert!(!fx.root.join("docs/x.md").exists());
    assert_eq!(fx.read("docs/y.md"), b"edited");
}

/// §11.3: a tracked `.claude/skills/x.md` is never written; X7: git status
/// shows no prepared file.
#[test]
fn local_tracked_paths_are_never_written_and_prepared_files_never_show_in_git() {
    let fx = Fx::new();
    git_repo(&fx.root);
    write(&fx.root, ".claude/skills/x.md", b"the repo's own\n");
    git(&fx.root, &["add", "."]);
    git(&fx.root, &["commit", "-qm", "tracked skill"]);
    fx.team(".claude/skills/x.md", b"team version");
    fx.team(".claude/skills/y/SKILL.md", b"team skill");
    fx.team("CLAUDE.md", b"# Team");
    let mut want = fx.want(&[".claude/skills"]);
    want.files.extend(fx.want(&["CLAUDE.md"]).files);
    want.files.sort_by(|a, b| a.dest.cmp(&b.dest));
    let report = fx.run(&want);
    assert_eq!(report.status, PrepareStatus::Ready);
    assert_eq!(report.skipped_tracked, vec![".claude/skills/x.md"]);
    assert_eq!(fx.read(".claude/skills/x.md"), b"the repo's own\n");
    assert_eq!(fx.read(".claude/skills/y/SKILL.md"), b"team skill");
    assert_eq!(git(&fx.root, &["status", "--porcelain"]), "", "X7");
}

/// §11.4: concurrent Step 2 in two worktrees of one repo leaves both
/// excluded (one block per workspace, written under the per-repo lock).
#[test]
fn concurrent_step_two_in_two_worktrees_of_one_repo_leaves_both_excluded() {
    let fx = Arc::new(Fx::new());
    git_repo(&fx.root);
    let second = fx._dirs.path().join("second");
    git(
        &fx.root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "two",
            second.to_str().unwrap(),
        ],
    );
    for i in 0..40 {
        fx.team(&format!("docs/f{i}.md"), format!("{i}").as_bytes());
    }
    let want = Arc::new(fx.want(&["docs"]));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = [fx.root.clone(), second.clone()]
        .into_iter()
        .map(|root| {
            let (fx, want, barrier) = (fx.clone(), want.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                fx.run_with(
                    &root,
                    &want,
                    Vec::new(),
                    false,
                    &crate::commands::worktree::NullEventSink,
                )
                .unwrap()
            })
        })
        .collect();
    for handle in handles {
        assert_eq!(handle.join().unwrap().status, PrepareStatus::Ready);
    }
    for root in [&fx.root, &second] {
        assert_eq!(
            git(root, &["status", "--porcelain"]),
            "",
            "{}",
            root.display()
        );
    }
    let exclude = std::fs::read_to_string(fx.root.join(".git/info/exclude")).unwrap();
    assert_eq!(
        exclude.matches("# >>> codesense managed workspace").count(),
        2
    );
}

/// §11.2: an upgraded workspace with a legacy lockfile. The lockfile seeds
/// the managed record: a stale unmodified legacy file is replaced, an edited
/// one is kept, and unwanted ones are reported as leftovers for Clean up.
#[test]
fn upgraded_workspace_with_a_legacy_lockfile_is_seeded() {
    let fx = Fx::new();
    // What the old projection wrote.
    write(&fx.root, ".claude/skills/a.md", b"old a");
    write(&fx.root, ".claude/skills/b.md", b"old b, then edited");
    write(&fx.root, "_bmad-output/epic/story.md", b"story");
    write(&fx.root, "_bmad-output/epic/notes.md", b"notes");
    let legacy = vec![
        (
            ".claude/skills/a.md".to_string(),
            hash(b"old a"),
            "legacy".to_string(),
        ),
        (
            ".claude/skills/b.md".to_string(),
            hash(b"old b"),
            "legacy".to_string(),
        ),
        (
            "_bmad-output/epic/story.md".to_string(),
            hash(b"story"),
            "legacy".to_string(),
        ),
        (
            "_bmad-output/epic/notes.md".to_string(),
            hash(b"notes"),
            "legacy".to_string(),
        ),
    ];
    fx.team(".claude/skills/a.md", b"new a");
    fx.team(".claude/skills/b.md", b"new b");
    let report = fx
        .run_with(
            &fx.root,
            &fx.want(&[".claude/skills"]),
            legacy,
            false,
            &crate::commands::worktree::NullEventSink,
        )
        .unwrap();
    assert_eq!(fx.read(".claude/skills/a.md"), b"new a");
    assert_eq!(fx.read(".claude/skills/b.md"), b"old b, then edited");
    assert_eq!(report.kept_modified, vec![".claude/skills/b.md"]);
    assert_eq!(report.leftovers, 2);
    assert!(fx.root.join("_bmad-output/epic/story.md").is_file());
    assert!(report.startup_note("team default").contains(
        "2 files that automatic sync copied here before are no longer in this agent's context"
    ));
}

/// A hydrate_worktree `docs` set survives a later launch of an agent whose
/// want set has no `docs`. SIMULATED: Step 3 is modelled as an engine run
/// with a `step3:` origin; until SW-XS-8 moves selected hydrate onto this
/// engine, real selected writes do not enter the managed record (XS-8 owns
/// that, noted on its story).
#[test]
fn a_hydrated_docs_set_survives_a_launch_without_docs() {
    let fx = Fx::new();
    fx.team("docs/guide.md", b"guide");
    fx.team(".claude/skills/s.md", b"skill");
    // Step 3 (hydrate_worktree docs) went through the engine.
    let legacy = || Ok(Vec::new());
    let docs = fx.want(&["docs"]);
    prepare(PrepareRequest {
        home: fx.home.path(),
        principal: &principal(),
        destination: Destination::Local { root: &fx.root },
        want: &docs,
        origin: "step3:op-1".into(),
        workspace: "w".into(),
        agent: "hydrate".into(),
        legacy: &legacy,
        prune: false,
        progress: &|_| {},
        validate: &|| Ok(()),
        explicit: Default::default(),
    })
    .unwrap();
    fx.run(&fx.want(&[".claude/skills"]));
    assert_eq!(fx.read("docs/guide.md"), b"guide");
    assert_eq!(fx.read(".claude/skills/s.md"), b"skill");
}

/// §11.11 + X5: the backend event sequence. Diff, then transfer, then apply,
/// then one `ready` — and `ready` only after every file is in place.
#[test]
fn progress_events_end_with_ready_only_after_step_two_completes() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    fx.team("docs/b.md", b"b");
    let sink = RecordingEventSink::default();
    let root = fx.root.clone();
    struct Checking(RecordingEventSink, PathBuf);
    impl EventSink for Checking {
        fn emit(&self, channel: &str, payload: serde_json::Value) {
            if payload["phase"] == "ready" {
                assert!(self.1.join("docs/a.md").is_file() && self.1.join("docs/b.md").is_file());
            }
            self.0.emit(channel, payload);
        }
    }
    let checking = Checking(sink.clone(), root.clone());
    fx.run_with(&root, &fx.want(&["docs"]), Vec::new(), false, &checking)
        .unwrap();
    let phases = sink.phases();
    assert_eq!(phases.first().map(String::as_str), Some("diff"));
    assert_eq!(phases.last().map(String::as_str), Some("ready"));
    assert_eq!(phases.iter().filter(|p| *p == "ready").count(), 1);
    let first = |name: &str| phases.iter().position(|p| p == name).unwrap();
    assert!(first("diff") < first("transfer") && first("transfer") < first("apply"));
    assert!(first("apply") < first("ready"));
    let events = sink.0.lock().unwrap();
    let ready = &events.last().unwrap().1;
    for key in ["workspace", "agent", "files", "bytes", "total"] {
        assert!(ready.get(key).is_some(), "{key}");
    }
    assert_eq!(ready["agent"], "Barb");
    // A second run with nothing changed: diff, then ready, no transfer.
    drop(events);
    let again = RecordingEventSink::default();
    fx.run_with(&root, &fx.want(&["docs"]), Vec::new(), false, &again)
        .unwrap();
    assert_eq!(again.phases(), vec!["diff", "ready"]);
}

/// §11.7: Step 2 is `Ready` only when no entry failed.
#[cfg(unix)]
#[test]
fn ready_only_when_no_entry_failed() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    fx.team("locked/b.md", b"b");
    std::fs::create_dir_all(fx.root.join("locked")).unwrap();
    std::fs::set_permissions(
        fx.root.join("locked"),
        std::fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    let sink = RecordingEventSink::default();
    let mut want = fx.want(&["docs"]);
    want.files.extend(fx.want(&["locked"]).files);
    let report = fx
        .run_with(&fx.root, &want, Vec::new(), false, &sink)
        .unwrap();
    std::fs::set_permissions(
        fx.root.join("locked"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(report.status, PrepareStatus::Failed);
    assert_eq!(report.failed.len(), 1);
    assert!(report.failure().unwrap().contains("locked/b.md"));
    assert!(!sink.phases().contains(&"ready".to_string()));
    assert_eq!(sink.phases().last().map(String::as_str), Some("failed"));
}

/// §11.5: Step 2 is skipped when the destination is the team content root.
#[test]
fn the_team_content_root_is_never_prepared() {
    let fx = Fx::new();
    fx.team(".claude/skills/s.md", b"s");
    let want = fx.want(&[".claude/skills"]);
    let report = fx
        .run_with(
            &fx.content,
            &want,
            Vec::new(),
            false,
            &crate::commands::worktree::NullEventSink,
        )
        .unwrap();
    assert_eq!(report.status, PrepareStatus::Skipped);
    assert!(report.is_ready());
}

/// A pinned startup file whose source changed after the launch was
/// prepared fails the run before anything is written.
#[test]
fn a_changed_pinned_startup_file_fails_before_any_write() {
    let fx = Fx::new();
    fx.team("CLAUDE.md", b"now");
    let mut want = fx.want(&["CLAUDE.md"]);
    want.files[0].pinned = Some(hash(b"then"));
    let error = fx
        .run_with(
            &fx.root,
            &want,
            Vec::new(),
            false,
            &crate::commands::worktree::NullEventSink,
        )
        .unwrap_err();
    assert!(error.starts_with("newIntentRequired:"));
    assert!(!fx.root.join("CLAUDE.md").exists());
}

/// MUST 2: the tracked-path guard fails CLOSED — a `.git` whose index can't
/// be read is an error and nothing is written.
#[test]
fn an_unreadable_git_index_fails_closed() {
    let fx = Fx::new();
    std::fs::write(fx.root.join(".git"), b"gitdir: /nowhere/at/all\n").unwrap();
    fx.team("docs/a.md", b"a");
    let error = fx
        .run_with(
            &fx.root,
            &fx.want(&["docs"]),
            Vec::new(),
            false,
            &crate::commands::worktree::NullEventSink,
        )
        .unwrap_err();
    assert!(error.contains("Git index"), "{error}");
    assert!(!fx.root.join("docs/a.md").exists());
}

/// MUST 3 (local; restores the deleted xs1 "every launch restores the
/// runtime exclude lines" proof): a block someone dropped from
/// `info/exclude` is put back on the next launch even when no file changed.
#[test]
fn local_every_launch_restores_the_exclude_block() {
    let fx = Fx::new();
    git_repo(&fx.root);
    fx.team("docs/a.md", b"a");
    fx.run(&fx.want(&["docs"]));
    assert_eq!(git(&fx.root, &["status", "--porcelain"]), "");
    let exclude = fx.root.join(".git/info/exclude");
    std::fs::write(&exclude, b"# user line\n").unwrap();
    assert_ne!(
        git(&fx.root, &["status", "--porcelain"]),
        "",
        "block dropped"
    );
    let report = fx.run(&fx.want(&["docs"]));
    assert!(report.written.is_empty());
    assert_eq!(
        git(&fx.root, &["status", "--porcelain"]),
        "",
        "block restored"
    );
    let text = std::fs::read_to_string(&exclude).unwrap();
    assert!(text.starts_with("# user line\n"), "user lines kept: {text}");
}

/// MUST 3/4 (local): a hand-edited block is never replaced by a launch; with
/// files to copy nothing is written and the run fails with the Repair action.
#[test]
fn local_hand_edited_block_fails_with_repair_and_writes_nothing() {
    let fx = Fx::new();
    git_repo(&fx.root);
    fx.team("docs/a.md", b"a");
    fx.run(&fx.want(&["docs"]));
    let exclude = fx.root.join(".git/info/exclude");
    let text = std::fs::read_to_string(&exclude).unwrap();
    std::fs::write(&exclude, text.replace("/docs/a.md", "docs/*")).unwrap();
    fx.team("docs/b.md", b"b");
    let report = fx.run(&fx.want(&["docs"]));
    assert_eq!(report.status, PrepareStatus::Failed);
    assert!(report.failure().unwrap().contains("Repair"));
    assert!(!fx.root.join("docs/b.md").exists());
}

/// MUST 5: pinned startup files the workspace holds differently (edited by
/// the agent, unmanaged, tracked) never block the launch, but are reported
/// as `pinnedDrift`, with a warning event and a startup-note line.
#[test]
fn pinned_drift_is_reported_and_never_blocks() {
    let fx = Fx::new();
    fx.team("CLAUDE.md", b"# Team");
    fx.team("docs/a.md", b"a");
    let mut want = fx.want(&["CLAUDE.md"]);
    want.files[0].pinned = Some(hash(b"# Team"));
    want.files.extend(fx.want(&["docs"]).files);
    fx.run(&want);
    // The agent edits its own CLAUDE.md.
    write(&fx.root, "CLAUDE.md", b"# Team\n\nmy own notes");
    let sink = RecordingEventSink::default();
    let report = fx
        .run_with(&fx.root, &want, Vec::new(), false, &sink)
        .unwrap();
    assert_eq!(report.status, PrepareStatus::Ready);
    assert_eq!(report.pinned_drift, vec!["CLAUDE.md"]);
    assert_eq!(fx.read("CLAUDE.md"), b"# Team\n\nmy own notes");
    let phases = sink.phases();
    assert_eq!(&phases[phases.len() - 2..], ["warning", "ready"]);
    let events = sink.0.lock().unwrap();
    assert_eq!(
        events.last().unwrap().1["summary"]["pinnedDrift"][0],
        "CLAUDE.md"
    );
    assert!(report
        .startup_note("team")
        .contains("1 pinned file differs from the agent definition: CLAUDE.md. It was kept"));
}

/// SHOULD 8 (racy-clean): a destination whose mtime is not a tick older than
/// the record's save is re-read even though its stat matches.
#[test]
fn a_stat_within_one_tick_of_the_record_save_is_rehashed() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    let want = fx.want(&["docs"]);
    fx.run(&want);
    assert_eq!(fx.run(&want).hashed, 0);
    let identity = crate::content_engine::context_lifecycle::local_identity(&fx.root);
    let path = record::record_path(fx.home.path(), &principal(), &identity);
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mtime = json["entries"]["docs/a.md"]["destStat"]["mtimeNs"]
        .as_i64()
        .unwrap();
    json["savedAtNs"] = serde_json::json!(mtime + 1);
    std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(fx.run(&want).hashed >= 1, "racy entry re-read");
}

/// SHOULD 9: nothing is written into a nested repository (§11.6).
#[test]
fn nested_repositories_are_skipped() {
    let fx = Fx::new();
    std::fs::create_dir_all(fx.root.join("docs/.git")).unwrap();
    fx.team("docs/a.md", b"a");
    let report = fx.run(&fx.want(&["docs"]));
    assert_eq!(report.status, PrepareStatus::Ready);
    assert!(report.skipped_other[0].1.contains("nested repository"));
    assert!(!fx.root.join("docs/a.md").exists());
}

/// SHOULD 14: an identical file CodeSense did not write is not adopted (not
/// managed, not excluded) and never chmodded.
#[cfg(unix)]
#[test]
fn identical_unmanaged_files_are_not_adopted_or_chmodded() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new();
    git_repo(&fx.root);
    fx.team("docs/a.md", b"same");
    write(&fx.root, "docs/a.md", b"same");
    std::fs::set_permissions(
        fx.root.join("docs/a.md"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut want = fx.want(&["docs"]);
    want.files[0].mode = 0o755;
    let report = fx.run(&want);
    assert!(report.written.is_empty() && report.chmodded.is_empty());
    let identity = crate::content_engine::context_lifecycle::local_identity(&fx.root);
    let record = ManagedRecord::load(fx.home.path(), &principal(), &identity);
    assert!(!record.entries.contains_key("docs/a.md"));
    assert_eq!(
        std::fs::metadata(fx.root.join("docs/a.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        git(&fx.root, &["status", "--porcelain"]).contains("docs/"),
        "the user's file shows"
    );
}

/// SHOULD 16: a team file that vanished after the want set was resolved is a
/// per-entry skip; the rest is prepared.
#[test]
fn a_vanished_source_skips_only_its_entry() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    fx.team("docs/b.md", b"b");
    let want = fx.want(&["docs"]);
    std::fs::remove_file(fx.content.join("docs/a.md")).unwrap();
    let report = fx.run(&want);
    assert_eq!(report.status, PrepareStatus::Ready);
    assert_eq!(report.skipped_other[0].0, "docs/a.md");
    assert_eq!(fx.read("docs/b.md"), b"b");
}

/// SHOULD 16: authority is re-checked right before the first write.
#[test]
fn authority_is_validated_before_writes() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    let legacy = || Ok(Vec::new());
    let error = prepare(PrepareRequest {
        home: fx.home.path(),
        principal: &principal(),
        destination: Destination::Local { root: &fx.root },
        want: &fx.want(&["docs"]),
        origin: "step2:x".into(),
        workspace: "w".into(),
        agent: "a".into(),
        legacy: &legacy,
        prune: false,
        progress: &|_| {},
        validate: &|| Err("workspace ownership changed".into()),
        explicit: Default::default(),
    })
    .unwrap_err();
    assert!(error.contains("ownership changed"));
    assert!(!fx.root.join("docs/a.md").exists());
}

/// Low: an add never replaces a file that appeared after the diff
/// (RENAME_EXCL); one that already holds the wanted bytes is a success.
#[test]
fn an_add_never_replaces_a_file_that_appeared() {
    let fx = Fx::new();
    let tmp = fx.root.join(".a.md.codesense-prep.x");
    std::fs::write(&tmp, b"team").unwrap();
    write(&fx.root, "a.md", b"user");
    let error =
        local::land(&fx.root, "a.md", &tmp, diff::Step::Add, None, hash(b"team")).unwrap_err();
    assert!(error.contains("appeared"));
    assert_eq!(fx.read("a.md"), b"user");
    write(&fx.root, "a.md", b"team");
    local::land(&fx.root, "a.md", &tmp, diff::Step::Add, None, hash(b"team")).unwrap();
}

/// Low: temp files a cut run left are swept once they are old.
#[test]
fn stale_prep_temp_files_are_swept() {
    let fx = Fx::new();
    let old = fx.root.join(".a.md.codesense-prep.old");
    let fresh = fx.root.join(".b.md.codesense-prep.new");
    std::fs::write(&old, b"x").unwrap();
    std::fs::write(&fresh, b"x").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
    local::sweep_stale_temps(&fx.root, &mut Default::default());
    assert!(!old.exists() && fresh.exists());
}

/// Verification: the legacy lockfile adapter reads real store rows.
#[test]
fn legacy_rows_local_reads_the_stored_lockfile() {
    use crate::content_engine::lockfile::{Lockfile, MaterializedEntry, SCHEMA_VERSION};
    use crate::content_engine::sync_state_store::{default_store_path, SyncStateStore};
    let fx = Fx::new();
    let ctx = ctx(&fx);
    let root = fx.root.canonicalize().unwrap();
    let lock = Lockfile {
        schema_version: SCHEMA_VERSION,
        org_slug: ctx.org_slug.clone(),
        team_name: ctx.team_name.clone(),
        code_repo_path: root.clone(),
        entries: vec![MaterializedEntry {
            logical_name: ".claude/skills/a.md".into(),
            dest_path: root.join(".claude/skills/a.md"),
            source_layer: crate::content_engine::LayerId::TeamProjection,
            source_path: fx.content.join(".claude/skills/a.md"),
            last_materialized_hash: hash(b"old"),
            last_materialized_mtime_ns: 0,
            projection_key: Some(".claude".into()),
            source_oid: None,
            last_materialized_size: Some(3),
        }],
    };
    let store = SyncStateStore::open(default_store_path(fx.home.path())).unwrap();
    store
        .upsert(
            42,
            &crate::domain::SourceRepoSlug::new("acme", "code").unwrap(),
            &lock,
        )
        .unwrap();
    let rows = legacy_rows_local(&ctx, &root).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, ".claude/skills/a.md");
    assert_eq!(rows[0].1, hash(b"old"));
}

fn ctx(fx: &Fx) -> crate::content_engine::ResolverContext {
    crate::content_engine::ResolverContext {
        context_root: None,
        codesense_home: fx.home.path().into(),
        user_id: 42,
        org_slug: "o/c".into(),
        team_name: Some("t".into()),
    }
}

/// Verification: a plain `claude` / `codex` launch's want set is its
/// runtime's default context (SPEC §12.2).
#[test]
fn plain_runtime_want_sets_are_the_runtime_defaults() {
    use crate::agents::resolve::tests::{alice, team_content, ORG, TEAM};
    let home = tempfile::tempdir().unwrap();
    team_content(home.path(), &alice());
    let target = crate::agents::ManifestTarget::team(ORG, TEAM);
    let dests = |runtime| {
        WantSet::for_runtime(home.path(), &alice(), runtime, &target, false)
            .unwrap()
            .files
            .into_iter()
            .map(|f| f.dest)
            .collect::<Vec<_>>()
    };
    let claude = dests(codesense_pty_core::agent_cli::AgentCli::Claude);
    assert!(claude.contains(&"CLAUDE.md".to_string()));
    assert!(claude.contains(&".claude/skills/bmad-build/SKILL.md".to_string()));
    assert!(claude.contains(&".claude/hooks/team-hook.sh".to_string()));
    assert!(!claude
        .iter()
        .any(|d| d.starts_with(".agents/") || d == ".mcp.json"));
    let codex = dests(codesense_pty_core::agent_cli::AgentCli::Codex);
    assert_eq!(codex, vec![".agents/skills/review/SKILL.md".to_string()]);
}

/// A legacy lockfile that can't be read (busy store) leaves the record
/// unseeded, so the next run seeds it — never silently "nothing to seed".
#[test]
fn an_unreadable_legacy_lockfile_is_seeded_later() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    let identity = crate::content_engine::context_lifecycle::local_identity(&fx.root);
    let busy = || Err("database is locked".to_string());
    prepare(PrepareRequest {
        home: fx.home.path(),
        principal: &principal(),
        destination: Destination::Local { root: &fx.root },
        want: &fx.want(&["docs"]),
        origin: "step2:x".into(),
        workspace: "w".into(),
        agent: "a".into(),
        legacy: &busy,
        prune: false,
        progress: &|_| {},
        validate: &|| Ok(()),
        explicit: Default::default(),
    })
    .unwrap();
    assert!(!ManagedRecord::load(fx.home.path(), &principal(), &identity).seeded);
    fx.run_with(
        &fx.root,
        &fx.want(&["docs"]),
        vec![("old.md".into(), hash(b"o"), "legacy".into())],
        false,
        &crate::commands::worktree::NullEventSink,
    )
    .unwrap();
    let record = ManagedRecord::load(fx.home.path(), &principal(), &identity);
    assert!(record.seeded && record.entries.contains_key("old.md"));
}

/// The `agent-prepare-progress` contract the frontend consumes (XS-10): a
/// golden fixture of one real run's event sequence, checked in at
/// `src/tauri/fixtures/agent-prepare-progress.golden.json` and read by the
/// TypeScript contract test. Regenerate with `UPDATE_GOLDEN=1`.
#[test]
fn agent_prepare_progress_golden_fixture() {
    let fx = Fx::new();
    fx.team("CLAUDE.md", b"# Team");
    fx.team("docs/a.md", b"a");
    let mut want = fx.want(&["CLAUDE.md"]);
    want.files[0].pinned = Some(hash(b"# Team"));
    want.files.extend(fx.want(&["docs"]).files);
    fx.run(&want);
    write(&fx.root, "CLAUDE.md", b"edited");
    fx.team("docs/b.md", b"b");
    let want = {
        let mut w = fx.want(&["CLAUDE.md"]);
        w.files[0].pinned = Some(hash(b"# Team"));
        w.files.extend(fx.want(&["docs"]).files);
        w
    };
    let sink = RecordingEventSink::default();
    fx.run_with(&fx.root, &want, Vec::new(), false, &sink)
        .unwrap();
    let events: Vec<serde_json::Value> = sink
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(_, payload)| {
            let mut payload = payload.clone();
            // Machine-specific values are normalized.
            payload["workspace"] = "/workspace".into();
            if let Some(summary) = payload.get_mut("summary") {
                summary["elapsedMs"] = 0.into();
            }
            if payload.get("note").is_some() {
                let note = payload["note"].as_str().unwrap().to_string();
                let note = regex::Regex::new(r"in [0-9.]+ s")
                    .unwrap()
                    .replace(&note, "in 0.0 s");
                payload["note"] = note.into_owned().into();
            }
            payload
        })
        .collect();
    let golden = serde_json::json!({
        "event": PREPARE_PROGRESS_EVENT,
        "sequence": events,
    });
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../src/tauri/fixtures/agent-prepare-progress.golden.json");
    let rendered = serde_json::to_string_pretty(&golden).unwrap() + "\n";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &rendered).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap_or_default(),
        rendered,
        "the event contract changed: rerun with UPDATE_GOLDEN=1 and update the TS type"
    );
}

/// r2 MUST: a prepared worktree removed WITHOUT discard consent (no
/// `--force`) succeeds and is cleaned up — its block and record go only after
/// the removal; a failed removal leaves both intact. Clearing the block first
/// (the r1 order) makes the prepared files untracked, and a non-forced
/// `git worktree remove` refuses them.
#[test]
fn teardown_removes_the_worktree_first_then_its_block_and_record() {
    let fx = Fx::new();
    git_repo(&fx.root);
    fx.team("docs/a.md", b"a");
    let want = fx.want(&["docs"]);
    let worktree = |name: &str| {
        let path = fx._dirs.path().join(name);
        git(
            &fx.root,
            &["worktree", "add", "-q", "-b", name, path.to_str().unwrap()],
        );
        fx.run_with(
            &path,
            &want,
            Vec::new(),
            false,
            &crate::commands::worktree::NullEventSink,
        )
        .unwrap();
        assert_eq!(git(&path, &["status", "--porcelain"]), "");
        path
    };
    let exclude = fx.root.join(".git/info/exclude");
    let block_of = |path: &Path| {
        let id = workspace_id(&crate::content_engine::context_lifecycle::local_identity(
            path,
        ));
        format!("# >>> codesense managed workspace {id}")
    };
    let record_of = |path: &Path| {
        ManagedRecord::exists(
            fx.home.path(),
            &principal(),
            &crate::content_engine::context_lifecycle::local_identity(path),
        )
    };
    let remove = |path: &Path| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&fx.root)
            .args(["worktree", "remove", path.to_str().unwrap()])
            .output()
            .unwrap();
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    };

    // The r1 order fails: without the block, the prepared files are untracked.
    let old_order = worktree("old-order");
    crate::content_engine::git_exclude::replace_workspace_block(
        &old_order,
        &workspace_id(&crate::content_engine::context_lifecycle::local_identity(
            &old_order,
        )),
        &[],
    )
    .unwrap();
    assert!(
        remove(&old_order).is_err(),
        "untracked prepared files refuse a plain remove"
    );

    // The fixed order: removed, then cleaned up.
    let prepared = worktree("prepared");
    let (block, had_record) = (block_of(&prepared), record_of(&prepared));
    assert!(had_record && std::fs::read_to_string(&exclude).unwrap().contains(&block));
    teardown_local_destination(fx.home.path(), &prepared, || remove(&prepared)).unwrap();
    assert!(!prepared.exists());
    assert!(!std::fs::read_to_string(&exclude).unwrap().contains(&block));
    let identity_file = record::record_path(
        fx.home.path(),
        &principal(),
        &DestinationIdentity::Local {
            root: prepared
                .parent()
                .unwrap()
                .canonicalize()
                .unwrap()
                .join("prepared"),
        },
    );
    assert!(!identity_file.exists());

    // A failed removal leaves block and record intact.
    let kept = worktree("kept");
    let block = block_of(&kept);
    let error =
        teardown_local_destination(fx.home.path(), &kept, || Err("refused".into())).unwrap_err();
    assert_eq!(error, "refused");
    assert!(std::fs::read_to_string(&exclude).unwrap().contains(&block));
    assert!(record_of(&kept));
    assert_eq!(git(&kept, &["status", "--porcelain"]), "");
}

// ---------------------------------------------------------------- remote

/// An in-memory machine workspace answering `manifest()` / `apply_batch()`
/// with the XS-6 semantics — every write, chmod and remove checked against
/// its pre-hash, the exclude block reported by the manifest and written by
/// compare-and-swap — counting both calls.
pub(crate) struct Machine {
    pub(crate) identity: DestinationIdentity,
    files: Mutex<BTreeMap<String, (Vec<u8>, u32)>>,
    tracked: Vec<String>,
    exclude: Mutex<Vec<String>>,
    /// The block reports as hand-edited.
    hand_edited: Mutex<bool>,
    /// Set the block to this just before the next Apply (another writer).
    exclude_race: Mutex<Option<Vec<String>>>,
    /// Apply the next batch, then lose its reply; the next manifest
    /// recovers it.
    pub(crate) lose_reply: Mutex<bool>,
    recovered: Mutex<Option<RecoveredBatch>>,
    manifests: Mutex<usize>,
    batches: Mutex<Vec<DestinationBatch>>,
}

impl Machine {
    pub(crate) fn new() -> Self {
        Self {
            identity: DestinationIdentity::Remote(RemoteDestinationIdentity {
                owner_id: 42,
                machine_id: "devbox".into(),
                server_id: "server".into(),
                workspace_handle: format!("ws-{}", uuid::Uuid::new_v4()),
            }),
            files: Mutex::default(),
            tracked: Vec::new(),
            exclude: Mutex::default(),
            hand_edited: Mutex::new(false),
            exclude_race: Mutex::new(None),
            lose_reply: Mutex::new(false),
            recovered: Mutex::new(None),
            manifests: Mutex::new(0),
            batches: Mutex::default(),
        }
    }
    fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.files.lock().unwrap().get(path).map(|(b, _)| b.clone())
    }
    pub(crate) fn put(&self, path: &str, bytes: &[u8]) {
        self.files
            .lock()
            .unwrap()
            .insert(path.into(), (bytes.to_vec(), 0o644));
    }
}

fn applied() -> MemberOutcome {
    MemberOutcome::Effect {
        outcome: EffectOutcome::Applied {
            observation: WireObservation::Absent,
        },
    }
}
fn conflict() -> MemberOutcome {
    MemberOutcome::Effect {
        outcome: EffectOutcome::Conflict {
            observation: WireObservation::Absent,
        },
    }
}

impl DestinationIo for Machine {
    fn manifest(
        &self,
        prefixes: &[String],
        _limit: usize,
        _progress: &mut dyn FnMut(&ManifestProgress),
    ) -> Result<CompleteManifest, DestinationBatchError> {
        *self.manifests.lock().unwrap() += 1;
        let files = self
            .files
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| {
                prefixes
                    .iter()
                    .any(|p| *path == p || path.starts_with(&format!("{p}/")))
            })
            .map(|(path, (bytes, mode))| ManifestEntry {
                path: path.clone(),
                kind: EntryKind::File,
                // A file over the member limit is listed unhashed here, the
                // case a mislabel would call "unreadable".
                hash: (bytes.len() <= codesense_pty_core::remote_files::MAX_FILE_BYTES)
                    .then(|| hash(bytes)),
                size: bytes.len() as u64,
                mode: *mode,
                tracked: self.tracked.contains(path),
                oversize: bytes.len() > codesense_pty_core::remote_files::MAX_FILE_BYTES,
                disk_name: None,
                reason: None,
            })
            .collect();
        let workspace_exclude = Some(if *self.hand_edited.lock().unwrap() {
            WorkspaceExclude::HandEdited {
                reason: "a foreign line".into(),
            }
        } else {
            WorkspaceExclude::Paths {
                paths: self.exclude.lock().unwrap().clone(),
            }
        });
        Ok(CompleteManifest {
            prefixes: prefixes.to_vec(),
            files,
            skipped: Vec::new(),
            progress: ManifestProgress::default(),
            recovered: self.recovered.lock().unwrap().take(),
            workspace_exclude,
        })
    }
    fn reconcile_pending(&self) -> Result<Option<RecoveredBatch>, DestinationBatchError> {
        Ok(self.recovered.lock().unwrap().take())
    }
    fn apply_batch(
        &self,
        batch: &DestinationBatch,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<DestinationBatchOutcome, DestinationBatchError> {
        self.batches.lock().unwrap().push(batch.clone());
        let total: u64 = batch.writes.iter().map(|w| w.bytes.len() as u64).sum();
        if !progress(total, total) {
            return Err(DestinationBatchError::Cancelled);
        }
        if let Some(race) = self.exclude_race.lock().unwrap().take() {
            *self.exclude.lock().unwrap() = race;
        }
        let mut operations = Vec::new();
        let mut exclude_paths = None;
        if let Some(block) = &batch.exclude {
            let mut current = self.exclude.lock().unwrap();
            let mut expected = block.expected.clone();
            expected.sort();
            let mut have = current.clone();
            have.sort();
            if !block.repair && have != expected {
                operations.push(OperationResult {
                    kind: OperationKind::ExcludeBlock,
                    index: 0,
                    operation_id: block.operation_id.to_string(),
                    outcome: conflict(),
                });
                return Ok(DestinationBatchOutcome {
                    phase: TransferPhase::Finished,
                    writes: batch
                        .writes
                        .iter()
                        .enumerate()
                        .map(|(index, w)| MemberResult {
                            index,
                            operation_id: w.operation_id.to_string(),
                            outcome: MemberOutcome::Skipped {
                                reason: "exclude changed".into(),
                            },
                        })
                        .collect(),
                    operations,
                    exclude_paths: None,
                    recovered: None,
                });
            }
            *current = block.paths.clone();
            exclude_paths = Some(block.paths.clone());
            operations.push(OperationResult {
                kind: OperationKind::ExcludeBlock,
                index: 0,
                operation_id: block.operation_id.to_string(),
                outcome: applied(),
            });
        }
        let mut files = self.files.lock().unwrap();
        let writes: Vec<MemberResult> = batch
            .writes
            .iter()
            .enumerate()
            .map(|(index, write)| {
                let current = files.get(write.path.as_str()).map(|(b, _)| hash(b));
                let ok = match write.expected {
                    ExpectedDestination::Absent => current.is_none(),
                    ExpectedDestination::Hash(h) => current == Some(h),
                };
                let outcome =
                    if write.bytes.len() > codesense_pty_core::remote_files::MAX_FILE_BYTES {
                        MemberOutcome::TooLarge {
                            size: write.bytes.len() as u64,
                            limit: codesense_pty_core::remote_files::MAX_FILE_BYTES as u64,
                        }
                    } else if ok {
                        files.insert(
                            write.path.as_str().into(),
                            (write.bytes.clone(), write.mode),
                        );
                        applied()
                    } else {
                        conflict()
                    };
                MemberResult {
                    index,
                    operation_id: write.operation_id.to_string(),
                    outcome,
                }
            })
            .collect();
        for (index, chmod) in batch.chmods.iter().enumerate() {
            let outcome = match files.get_mut(chmod.path.as_str()) {
                Some(entry) if hash(&entry.0) == chmod.expected_pre_hash => {
                    entry.1 = chmod.mode;
                    applied()
                }
                _ => conflict(),
            };
            operations.push(OperationResult {
                kind: OperationKind::Chmod,
                index,
                operation_id: chmod.operation_id.to_string(),
                outcome,
            });
        }
        for (index, remove) in batch.removes.iter().enumerate() {
            let outcome = match files.get(remove.path.as_str()) {
                None => applied(),
                Some(entry) if hash(&entry.0) == remove.expected_pre_hash => {
                    files.remove(remove.path.as_str());
                    applied()
                }
                Some(_) => conflict(),
            };
            operations.push(OperationResult {
                kind: OperationKind::Remove,
                index,
                operation_id: remove.operation_id.to_string(),
                outcome,
            });
        }
        if std::mem::take(&mut *self.lose_reply.lock().unwrap()) {
            *self.recovered.lock().unwrap() = Some(RecoveredBatch {
                batch_id: batch.batch_id,
                phase: TransferPhase::Finished,
                writes,
                operations,
                exclude_paths,
            });
            return Err(DestinationBatchError::Failed("the reply was lost".into()));
        }
        Ok(DestinationBatchOutcome {
            phase: TransferPhase::Finished,
            writes,
            operations,
            exclude_paths,
            recovered: None,
        })
    }
    fn identity(&self) -> &DestinationIdentity {
        &self.identity
    }
    fn validate_authority(&self) -> Result<(), String> {
        Ok(())
    }
    fn inspect(&self, _: &DestinationRelativePath) -> Result<DestinationObservation, String> {
        panic!("Step 2 never inspects per file (X2)")
    }
    fn read(
        &self,
        _: &DestinationRelativePath,
        _: Option<[u8; 32]>,
        _: usize,
    ) -> Result<(DestinationObservation, Vec<u8>), String> {
        panic!("Step 2 never reads per file (X2)")
    }
    fn enumerate(
        &self,
        _: Option<&DestinationRelativePath>,
        _: usize,
    ) -> Result<Vec<DestinationRelativePath>, String> {
        panic!("no per-prefix enumerate")
    }
    fn apply(
        &self,
        _: uuid::Uuid,
        _: &DestinationRelativePath,
        _: &ExpectedDestination,
        _: &DestinationEffect,
    ) -> Result<DestinationEffectOutcome, String> {
        panic!("Step 2 never applies per file (X2)")
    }
    fn reconcile(&self, _: uuid::Uuid) -> Result<DestinationEffectOutcome, String> {
        panic!("no per-effect reconcile")
    }
    fn tracked_paths(
        &self,
        _: Option<&DestinationRelativePath>,
        _: usize,
    ) -> Result<Vec<DestinationRelativePath>, String> {
        panic!("tracked comes with the manifest")
    }
    fn ensure_directories(
        &self,
        _: uuid::Uuid,
        _: &DestinationRelativePath,
    ) -> Result<DestinationEffectOutcome, String> {
        panic!("directories come with the batch")
    }
    fn read_managed_exclude(&self) -> Result<ManagedExcludeState, String> {
        panic!("the exclude block comes with the batch")
    }
    fn replace_managed_exclude(
        &self,
        _: uuid::Uuid,
        _: &ExpectedDestination,
        _: &[String],
    ) -> Result<DestinationEffectOutcome, String> {
        panic!("the exclude block comes with the batch")
    }
}

impl Fx {
    fn remote_result(
        &self,
        machine: &Machine,
        want: &WantSet,
        prune: bool,
        sink: &dyn EventSink,
    ) -> Result<PrepareReport, String> {
        let legacy = || Ok(Vec::new());
        let emit = progress::to_sink(sink);
        let report = prepare(PrepareRequest {
            home: self.home.path(),
            principal: &principal(),
            destination: Destination::Remote { io: machine },
            want,
            origin: "step2:o/t/barb".into(),
            workspace: "ws".into(),
            agent: "Barb".into(),
            legacy: &legacy,
            prune,
            progress: &emit,
            validate: &|| Ok(()),
            explicit: Default::default(),
        })?;
        progress::finish(&emit, &report, "ws", "Barb", "team default");
        Ok(report)
    }
    fn remote(&self, machine: &Machine, want: &WantSet, prune: bool) -> PrepareReport {
        self.remote_result(
            machine,
            want,
            prune,
            &crate::commands::worktree::NullEventSink,
        )
        .unwrap()
    }
}
/// AC1 (engine level; the wire count is in `manifest_wire_tests`): 2,000
/// declared files, nothing changed → ONE manifest call and no batch.
#[test]
fn remote_unchanged_is_one_manifest_call_and_no_batch() {
    let fx = Fx::new();
    for i in 0..2_000 {
        fx.team(
            &format!("docs/d{}/f{i}.md", i % 13),
            format!("{i}").as_bytes(),
        );
    }
    let machine = Machine::new();
    let want = fx.want(&["docs"]);
    let first = fx.remote(&machine, &want, false);
    assert_eq!(first.written.len(), 2_000);
    assert_eq!(machine.batches.lock().unwrap().len(), 1, "one batch");
    let second = fx.remote(&machine, &want, false);
    assert!(second.written.is_empty());
    assert_eq!(*machine.manifests.lock().unwrap(), 2);
    assert_eq!(machine.batches.lock().unwrap().len(), 1, "no second batch");
    assert_eq!(second.hashed, 0, "source stat cache");
}

/// AC2: 50 changed and 5 newly unwanted managed files → ONE Apply with the
/// 50 writes and (on an explicit prune) the 5 removes, and the exclude block
/// updated in that same Apply. A launch makes the 50 writes and no remove.
#[test]
fn remote_fifty_changes_and_five_unwanted_are_one_batch() {
    let fx = Fx::new();
    for i in 0..100 {
        fx.team(&format!("docs/f{i}.md"), b"v1");
    }
    let machine = Machine::new();
    fx.remote(&machine, &fx.want(&["docs"]), false);
    for i in 0..50 {
        fx.team(&format!("docs/f{i}.md"), b"v2");
    }
    for i in 95..100 {
        std::fs::remove_file(fx.content.join(format!("docs/f{i}.md"))).unwrap();
    }
    // A launch: writes only.
    let launch = fx.remote(&machine, &fx.want(&["docs"]), false);
    assert_eq!(launch.written.len(), 50);
    assert!(launch.removed.is_empty());
    let batches = machine.batches.lock().unwrap().clone();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[1].writes.len(), 50);
    assert!(batches[1].removes.is_empty());
    // Same shape again with a prune: the 50 writes and 5 removes together.
    for i in 0..50 {
        fx.team(&format!("docs/f{i}.md"), b"v3");
    }
    let prune = fx.remote(&machine, &fx.want(&["docs"]), true);
    let batches = machine.batches.lock().unwrap().clone();
    assert_eq!(batches.len(), 3, "one Apply");
    let last = &batches[2];
    assert_eq!(last.writes.len(), 50);
    assert_eq!(last.removes.len(), 5);
    let block = last.exclude.as_ref().expect("exclude in the same Apply");
    assert_eq!(block.paths.len(), 95);
    assert_eq!(prune.removed.len(), 5);
    assert!(machine.get("docs/f99.md").is_none());
}

/// AC3 + §11.3 on a machine: a modified managed file and a tracked file are
/// never written.
#[test]
fn remote_modified_and_tracked_files_are_never_written() {
    let fx = Fx::new();
    fx.team(".claude/skills/a.md", b"a1");
    fx.team(".claude/skills/x.md", b"team x");
    let mut machine = Machine::new();
    machine.tracked = vec![".claude/skills/x.md".into()];
    machine
        .files
        .lock()
        .unwrap()
        .insert(".claude/skills/x.md".into(), (b"repo x".to_vec(), 0o644));
    fx.remote(&machine, &fx.want(&[".claude/skills"]), false);
    machine
        .files
        .lock()
        .unwrap()
        .insert(".claude/skills/a.md".into(), (b"edited".to_vec(), 0o644));
    fx.team(".claude/skills/a.md", b"a2");
    let report = fx.remote(&machine, &fx.want(&[".claude/skills"]), false);
    assert_eq!(report.kept_modified, vec![".claude/skills/a.md"]);
    assert_eq!(report.skipped_tracked, vec![".claude/skills/x.md"]);
    assert_eq!(machine.get(".claude/skills/a.md").unwrap(), b"edited");
    assert_eq!(machine.get(".claude/skills/x.md").unwrap(), b"repo x");
}

/// MUST 4: a block holding paths the record doesn't know keeps them — the
/// new block is the union of what the machine reported and what is managed.
#[test]
fn remote_block_paths_the_record_does_not_know_are_never_dropped() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    let machine = Machine::new();
    *machine.exclude.lock().unwrap() = vec!["docs/other.md".into()];
    let report = fx.remote(&machine, &fx.want(&["docs"]), false);
    assert_eq!(report.status, PrepareStatus::Ready);
    assert_eq!(machine.batches.lock().unwrap().len(), 1);
    assert_eq!(
        *machine.exclude.lock().unwrap(),
        vec!["docs/a.md".to_string(), "docs/other.md".to_string()]
    );
}

/// MUST 4: the block changed between the manifest and the Apply (another
/// writer): nothing was written; the block is read again and the batch
/// resent against it, merged — never a blind `repair`.
#[test]
fn remote_block_changed_before_the_apply_is_merged_and_retried() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    let machine = Machine::new();
    *machine.exclude_race.lock().unwrap() = Some(vec!["docs/theirs.md".into()]);
    let report = fx.remote(&machine, &fx.want(&["docs"]), false);
    assert_eq!(report.status, PrepareStatus::Ready);
    let batches = machine.batches.lock().unwrap().clone();
    assert_eq!(batches.len(), 2);
    assert!(batches.iter().all(|b| !b.exclude.as_ref().unwrap().repair));
    assert_eq!(*machine.manifests.lock().unwrap(), 2);
    assert_eq!(
        *machine.exclude.lock().unwrap(),
        vec!["docs/a.md".to_string(), "docs/theirs.md".to_string()]
    );
    assert_eq!(machine.get("docs/a.md").unwrap(), b"a");
}

/// MUST 4: a hand-edited block is never replaced by a launch: with files to
/// copy, nothing is written and the run fails with the Repair action.
#[test]
fn remote_hand_edited_block_fails_with_repair_and_writes_nothing() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    let machine = Machine::new();
    *machine.hand_edited.lock().unwrap() = true;
    let report = fx.remote(&machine, &fx.want(&["docs"]), false);
    assert_eq!(report.status, PrepareStatus::Failed);
    assert!(report.failure().unwrap().contains("Repair"));
    assert!(machine.batches.lock().unwrap().is_empty());
    assert!(machine.get("docs/a.md").is_none());
}

/// MUST 3 (remote): a block someone dropped is restored on the next launch
/// even when no file changed — the manifest reports it, at no extra call.
#[test]
fn remote_every_launch_restores_the_exclude_block() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    let machine = Machine::new();
    fx.remote(&machine, &fx.want(&["docs"]), false);
    let calls = |m: &Machine| {
        (
            *m.manifests.lock().unwrap(),
            m.batches.lock().unwrap().len(),
        )
    };
    assert_eq!(calls(&machine), (1, 1));
    fx.remote(&machine, &fx.want(&["docs"]), false);
    assert_eq!(calls(&machine), (2, 1), "intact block: no batch");
    machine.exclude.lock().unwrap().clear();
    let report = fx.remote(&machine, &fx.want(&["docs"]), false);
    assert!(report.written.is_empty());
    assert_eq!(
        calls(&machine),
        (3, 2),
        "dropped block: one batch restores it"
    );
    assert_eq!(
        *machine.exclude.lock().unwrap(),
        vec!["docs/a.md".to_string()]
    );
}

/// MUST 1: a file over a machine's per-file limit is a per-entry skip
/// (`too large for a machine`), never a failure, and never sent; one already
/// on the machine is labelled so too, not "unreadable".
#[test]
fn remote_too_large_files_are_skipped_not_failed() {
    let fx = Fx::new();
    let big = vec![b'x'; codesense_pty_core::remote_files::MAX_FILE_BYTES + 1];
    fx.team("docs/big.bin", &big);
    fx.team("docs/a.md", b"a");
    let machine = Machine::new();
    let report = fx.remote(&machine, &fx.want(&["docs"]), false);
    assert_eq!(report.status, PrepareStatus::Ready);
    assert!(report.failed.is_empty());
    assert_eq!(report.skipped_other.len(), 1);
    assert_eq!(report.skipped_other[0].0, "docs/big.bin");
    assert!(report.skipped_other[0]
        .1
        .contains("too large for a machine"));
    let batches = machine.batches.lock().unwrap().clone();
    assert!(batches[0]
        .writes
        .iter()
        .all(|w| w.path.as_str() != "docs/big.bin"));
    assert_eq!(machine.get("docs/a.md").unwrap(), b"a");
    // One already on the machine (other bytes) is labelled too large too.
    let mut other = big.clone();
    other[0] = b'y';
    let fresh = Machine::new();
    fresh.put("docs/big.bin", &other);
    let report = fx.remote(&fresh, &fx.want(&["docs"]), false);
    let reason = &report
        .skipped_other
        .iter()
        .find(|(p, _)| p == "docs/big.bin")
        .unwrap()
        .1;
    assert!(reason.contains("too large for a machine"), "{reason}");
    assert!(report.failed.is_empty());
}

/// A batch whose reply was lost is learned from the next manifest's
/// recovery before the next diff: its writes become managed, nothing is
/// copied twice.
#[test]
fn remote_batch_with_a_lost_reply_is_recovered_into_the_record() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    let machine = Machine::new();
    *machine.lose_reply.lock().unwrap() = true;
    let first = fx.remote_result(
        &machine,
        &fx.want(&["docs"]),
        false,
        &crate::commands::worktree::NullEventSink,
    );
    assert!(first.is_err());
    // The machine applied it; the next run recovers instead of re-sending.
    fx.team("docs/a.md", b"a2");
    let second = fx.remote(&machine, &fx.want(&["docs"]), false);
    assert_eq!(
        second.written,
        vec!["docs/a.md"],
        "replaced as a managed file"
    );
    assert_eq!(machine.get("docs/a.md").unwrap(), b"a2");
    let record = ManagedRecord::load(fx.home.path(), &principal(), &machine.identity);
    assert!(record.pending.is_empty());
}

/// r2: a failed apply never drops an earlier unsettled batch — both stay
/// pending until a recovery names them.
#[test]
fn remote_a_failed_apply_keeps_an_earlier_unsettled_batch() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    let machine = Machine::new();
    *machine.lose_reply.lock().unwrap() = true;
    assert!(fx
        .remote_result(
            &machine,
            &fx.want(&["docs"]),
            false,
            &crate::commands::worktree::NullEventSink
        )
        .is_err());
    // The machine forgets the first recovery (another client settled its
    // journal), and the second batch's reply is lost too.
    machine.recovered.lock().unwrap().take();
    fx.team("docs/b.md", b"b");
    *machine.lose_reply.lock().unwrap() = true;
    assert!(fx
        .remote_result(
            &machine,
            &fx.want(&["docs"]),
            false,
            &crate::commands::worktree::NullEventSink
        )
        .is_err());
    let record = ManagedRecord::load(fx.home.path(), &principal(), &machine.identity);
    assert_eq!(record.pending.len(), 2, "the earlier batch is kept");
}

/// An empty want set asks for no manifest but still settles a pending batch.
#[test]
fn remote_empty_want_settles_a_pending_batch() {
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    let machine = Machine::new();
    *machine.lose_reply.lock().unwrap() = true;
    assert!(fx
        .remote_result(
            &machine,
            &fx.want(&["docs"]),
            false,
            &crate::commands::worktree::NullEventSink
        )
        .is_err());
    let manifests = *machine.manifests.lock().unwrap();
    fx.remote(&machine, &WantSet::default(), false);
    assert_eq!(
        *machine.manifests.lock().unwrap(),
        manifests,
        "no manifest for an empty want"
    );
    let record = ManagedRecord::load(fx.home.path(), &principal(), &machine.identity);
    assert!(record.pending.is_empty());
    assert!(record.entries.contains_key("docs/a.md"));
}

/// §11.11: a remote workspace with pending legacy effects still launches —
/// the rows are cleared and the destination admitted as explicit.
#[test]
fn remote_workspace_with_pending_legacy_effects_launches() {
    use crate::content_engine::sync_state_store::{default_store_path, SyncStateStore};
    let fx = Fx::new();
    fx.team("docs/a.md", b"a");
    let machine = Machine::new();
    let DestinationIdentity::Remote(remote) = machine.identity.clone() else {
        unreachable!()
    };
    let ctx = crate::content_engine::ResolverContext {
        context_root: None,
        codesense_home: fx.home.path().into(),
        user_id: 42,
        org_slug: "o/c".into(),
        team_name: Some("t".into()),
    };
    let slug = crate::domain::SourceRepoSlug::new("acme", "code").unwrap();
    let store = SyncStateStore::open(default_store_path(fx.home.path())).unwrap();
    store
        .prepare_destination_effect(&ctx, &slug, &remote, "docs/a.md", &serde_json::json!({}))
        .unwrap();
    assert!(
        store.admit_explicit_context(&machine.identity).is_err(),
        "blocked before"
    );
    let report = fx.remote(&machine, &fx.want(&["docs"]), false);
    assert_eq!(report.status, PrepareStatus::Ready);
    assert!(store
        .pending_destination_effects(&ctx, &slug, &remote)
        .unwrap()
        .is_empty());
    assert!(store.uses_explicit_context(&machine.identity).unwrap());
}

/// Verification: the remote legacy lockfile adapter reads real store rows.
#[test]
fn legacy_rows_remote_reads_the_stored_lockfile() {
    use crate::content_engine::lockfile::{Lockfile, MaterializedEntry, SCHEMA_VERSION};
    use crate::content_engine::sync_state_store::{default_store_path, SyncStateStore};
    let fx = Fx::new();
    let ctx = ctx(&fx);
    let machine = Machine::new();
    let DestinationIdentity::Remote(remote) = machine.identity.clone() else {
        unreachable!()
    };
    let slug = crate::domain::SourceRepoSlug::new("acme", "code").unwrap();
    let relative = DestinationRelativePath::try_from("docs/a.md".to_string()).unwrap();
    let lock: DestinationLockfile = Lockfile {
        schema_version: SCHEMA_VERSION,
        org_slug: ctx.org_slug.clone(),
        team_name: ctx.team_name.clone(),
        code_repo_path: machine.identity.clone(),
        entries: vec![MaterializedEntry {
            logical_name: "docs/a.md".into(),
            dest_path: DestinationReference::Remote {
                destination: remote.clone(),
                relative,
            },
            source_layer: crate::content_engine::LayerId::TeamProjection,
            source_path: fx.content.join("docs/a.md"),
            last_materialized_hash: hash(b"old"),
            last_materialized_mtime_ns: 0,
            projection_key: Some("docs".into()),
            source_oid: None,
            last_materialized_size: Some(3),
        }],
    };
    let store = SyncStateStore::open(default_store_path(fx.home.path())).unwrap();
    store.upsert_remote_target(&ctx, &slug, &lock).unwrap();
    let rows = legacy_rows_remote(&ctx, &slug, &remote).unwrap();
    assert_eq!(
        rows,
        vec![("docs/a.md".to_string(), hash(b"old"), rows[0].2.clone())]
    );
}

// ---------------------------------------------------------------- guards

/// AC6: no launch path constructs a remote projection destination. SW-XS-8
/// deleted the legacy remote projection engine (and with it
/// `ProjectionDestination`, guarded in `explicit_sync_projection_retirement_guard`),
/// so the allow-list is empty.
#[test]
fn no_remote_projection_destination_outside_the_allow_list() {
    let allowed: [&str; 0] = [];
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    for entry in walkdir::WalkDir::new(&src)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        let name = path.to_string_lossy();
        if path.extension().is_none_or(|e| e != "rs") || name.contains("test") {
            continue;
        }
        let relative = path
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(path).unwrap();
        let constructs = text.lines().any(|line| {
            let line = line.trim();
            !line.starts_with("//")
                && line.contains("Target::Remote(")
                && !line.contains("=>")
                && !line.starts_with("if let")
                && !line.contains("matches!")
        });
        if constructs && !allowed.contains(&relative.as_str()) {
            offenders.push(relative);
        }
    }
    assert!(
        offenders.is_empty(),
        "remote projection destinations built in {offenders:?}"
    );
}

/// SW-XS-11 AC-3: the startup note reads as SPEC §8.5 says, word for word,
/// counts agree with their nouns, and kept-modified files are named.
#[test]
fn startup_note_matches_spec_8_5_with_kept_modified_files() {
    let mut report = PrepareReport::skipped(String::new(), &WantSet::default());
    report.status = PrepareStatus::Ready;
    report.skipped_reason = None;
    report.wanted = 312;
    report.wanted_bytes = 4_300_000;
    report.elapsed_ms = 2_300;
    report.kept_modified = vec!["CLAUDE.md".into(), "docs/a.md".into()];
    assert_eq!(
        report.startup_note("team default"),
        "Prepared 312 files (4.1 MB) from team default in 2.3 s. 2 kept because you modified them: CLAUDE.md, docs/a.md. To get more, call hydrate_worktree."
    );

    report.wanted = 1;
    report.wanted_bytes = 6;
    report.kept_modified.clear();
    report.pinned_drift = vec!["CLAUDE.md".into()];
    report.leftovers = 1;
    let note = report.startup_note("team default");
    assert!(
        note.starts_with("Prepared 1 file (6 B) from team default"),
        "{note}"
    );
    assert!(note.contains("1 pinned file differs from the agent definition: CLAUDE.md."));
    assert!(note
        .contains(r#"call hydrate_worktree with paths:["CLAUDE.md"] and overwriteModified:true."#));
    assert!(!note.contains("can't restore"));
    assert!(note.contains("1 file that automatic sync copied here before is no longer"));
    assert!(
        note.contains("It stays until you delete it; list_hydrated shows it with origin legacy.")
    );
    assert!(!note.contains("prune"));

    // A pinned file the repo tracks can't be restored by a hydrate: the note
    // says so, and the hydrate it suggests names only the restorable ones.
    report.pinned_drift = vec!["AGENTS.md".into(), "CLAUDE.md".into()];
    report.skipped_tracked = vec!["AGENTS.md".into()];
    let note = report.startup_note("team default");
    assert!(note.contains("2 pinned files differ from the agent definition: AGENTS.md, CLAUDE.md."));
    assert!(note.contains(r#"paths:["CLAUDE.md"] and overwriteModified:true"#));
    assert!(note.contains("so a hydrate can't restore it: AGENTS.md."));
    for stale in ["file(s)", "projection", "cleaned up", "sync engine"] {
        assert!(
            !note.contains(stale),
            "the note still says `{stale}`: {note}"
        );
    }
}

// Shared exclude history is collected through the same entry point as prepare
// and publish, with real git-registered worktrees rather than invented live ids.
fn stale_workspace_id(root: &Path) -> String {
    workspace_id(&crate::content_engine::context_lifecycle::local_identity(
        root,
    ))
}

fn stale_workspace_block(id: &str) -> String {
    format!("# >>> codesense managed workspace {id}\n/docs/a.md\n# <<< codesense managed workspace {id}\n")
}

fn stale_workspace_written_block(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap();
    let id = stale_workspace_id(root);
    format!("# >>> codesense managed workspace {id}\n# root: {}\n/docs/a.md\n# <<< codesense managed workspace {id}\n", canonical.display())
}

fn stale_workspace_link(fx: &Fx, name: &str) -> PathBuf {
    let path = fx._dirs.path().join(name);
    git(
        &fx.root,
        &["worktree", "add", "-q", "-b", name, path.to_str().unwrap()],
    );
    path
}

fn stale_workspace_write(root: &Path) {
    local::write_block(root, &stale_workspace_id(root), &[], &["docs/a.md".into()]).unwrap();
}

#[test]
fn stale_workspace_collects_history_verbatim_and_restart() {
    let fx = Fx::new();
    git_repo(&fx.root);
    let a = stale_workspace_link(&fx, "gc-a");
    let b = stale_workspace_link(&fx, "gc-b");
    let exclude = fx.root.join(".git/info/exclude");
    let b_block = stale_workspace_block(&stale_workspace_id(&b)).replace('\n', "\r\n");
    let remote = stale_workspace_block("remote-handle");
    let plain = "# >>> codesense managed\r\n/plain\r\n# <<< codesense managed\r\n";
    let keep = format!("# user before\r\n{b_block}# between\r\n{remote}{plain}# user after");
    let seeded = format!(
        "# user before\r\n{}{b_block}{}# between\r\n{remote}{plain}{}# user after",
        stale_workspace_block("local-old1"),
        stale_workspace_block("local-old2"),
        stale_workspace_block("local-old3")
    );
    std::fs::write(&exclude, seeded).unwrap();
    stale_workspace_write(&a);
    let expected = format!("{keep}\n{}", stale_workspace_written_block(&a));
    assert_eq!(std::fs::read_to_string(&exclude).unwrap(), expected);
    let before = std::fs::metadata(&exclude).unwrap();
    stale_workspace_write(&a);
    assert_eq!(std::fs::read_to_string(&exclude).unwrap(), expected);
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        std::fs::metadata(&exclude).unwrap().ino(),
        before.ino(),
        "unchanged content must not be renamed"
    );
}

#[test]
fn stale_workspace_deleted_registered_directory() {
    let fx = Fx::new();
    git_repo(&fx.root);
    let deleted = stale_workspace_link(&fx, "gc-deleted");
    let block = stale_workspace_block(&stale_workspace_id(&deleted));
    let exclude = fx.root.join(".git/info/exclude");
    std::fs::write(&exclude, &block).unwrap();
    std::fs::remove_dir_all(&deleted).unwrap();
    assert!(git(&fx.root, &["worktree", "list", "--porcelain"]).contains("gc-deleted"));
    stale_workspace_write(&fx.root);
    assert!(!std::fs::read_to_string(&exclude).unwrap().contains(&block));
}

#[test]
fn stale_workspace_symlink_identities() {
    let fx = Fx::new();
    git_repo(&fx.root);
    let linked = stale_workspace_link(&fx, "gc-symlink");
    let alias = fx._dirs.path().join("alias");
    std::os::unix::fs::symlink(&linked, &alias).unwrap();
    // Register an alias spelling in this disposable repo, as can happen after
    // moving/repairing a worktree. Git's porcelain then exposes that raw path.
    let registration = fx.root.join(".git/worktrees/gc-symlink/gitdir");
    std::fs::write(registration, format!("{}\n", alias.join(".git").display())).unwrap();
    assert!(git(&fx.root, &["worktree", "list", "--porcelain"]).contains(alias.to_str().unwrap()));
    let canonical_block = stale_workspace_block(&stale_workspace_id(&alias));
    let raw_id = workspace_id(&DestinationIdentity::Local {
        root: alias.clone(),
    });
    assert_ne!(raw_id, stale_workspace_id(&alias));
    let raw_block = stale_workspace_block(&raw_id);
    let exclude = fx.root.join(".git/info/exclude");
    std::fs::write(&exclude, format!("{canonical_block}{raw_block}")).unwrap();
    stale_workspace_write(&fx.root);
    let text = std::fs::read_to_string(&exclude).unwrap();
    assert!(text.contains(&canonical_block));
    assert!(text.contains(&raw_block));
}

#[test]
fn stale_workspace_both_write_orders() {
    for c_first in [true, false] {
        let fx = Fx::new();
        git_repo(&fx.root);
        let a = stale_workspace_link(&fx, "gc-order-a");
        if !c_first {
            stale_workspace_write(&a);
        }
        let c = stale_workspace_link(&fx, "gc-order-c");
        stale_workspace_write(&c);
        if c_first {
            stale_workspace_write(&a);
        }
        let text = std::fs::read_to_string(fx.root.join(".git/info/exclude")).unwrap();
        assert!(text.contains(&stale_workspace_written_block(&c)));
        assert!(text.contains(&stale_workspace_written_block(&a)));
    }
}

#[test]
fn stale_workspace_foreign_markers_unchanged() {
    for broken in [
        "# >>> codesense managed workspace local-broken\n/path\n",
        "# <<< codesense managed workspace local-broken\n",
        "# >>> codesense managed workspace local-broken\n# >>> codesense managed workspace remote\n/path\n# <<< codesense managed workspace remote\n# <<< codesense managed workspace local-broken\n",
    ] {
        let fx = Fx::new();
        git_repo(&fx.root);
        let exclude = fx.root.join(".git/info/exclude");
        let foreign = format!("{}{broken}", stale_workspace_block("local-stale"));
        std::fs::write(&exclude, &foreign).unwrap();
        stale_workspace_write(&fx.root);
        assert_eq!(std::fs::read_to_string(exclude).unwrap(), format!("{foreign}{}", stale_workspace_written_block(&fx.root)));
    }
}

#[test]
fn stale_workspace_teardown_is_scoped_and_preserves_history() {
    let fx = Fx::new();
    git_repo(&fx.root);
    let removed = stale_workspace_link(&fx, "gc-teardown");
    stale_workspace_write(&removed);
    let exclude = fx.root.join(".git/info/exclude");
    let old = std::fs::read_to_string(&exclude).unwrap();
    let own = stale_workspace_written_block(&removed);
    let history = format!(
        "{}# user tail",
        stale_workspace_block("local-historical").replace('\n', "\r\n")
    );
    std::fs::write(&exclude, format!("{old}{history}")).unwrap();
    teardown_local_destination(fx.home.path(), &removed, || {
        git(&fx.root, &["worktree", "remove", removed.to_str().unwrap()]);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&exclude).unwrap(),
        format!("{}{history}", old.replace(&own, ""))
    );
}

#[test]
fn stale_workspace_unchanged_block_still_collects_history() {
    let fx = Fx::new();
    git_repo(&fx.root);
    let id = stale_workspace_id(&fx.root);
    let own = stale_workspace_block(&id);
    let exclude = fx.root.join(".git/info/exclude");
    std::fs::write(
        &exclude,
        format!("{}{}", stale_workspace_block("local-stale"), own),
    )
    .unwrap();
    let paths = vec!["docs/a.md".into()];
    local::write_block(&fx.root, &id, &paths, &paths).unwrap();
    let expected = stale_workspace_written_block(&fx.root);
    assert_eq!(std::fs::read_to_string(&exclude).unwrap(), expected);
    assert_eq!(local::actual_block(&fx.root, &id).unwrap().unwrap(), paths);
    use std::os::unix::fs::MetadataExt;
    let inode = std::fs::metadata(&exclude).unwrap().ino();
    local::write_block(&fx.root, &id, &paths, &paths).unwrap();
    assert_eq!(std::fs::read_to_string(&exclude).unwrap(), expected);
    assert_eq!(std::fs::metadata(&exclude).unwrap().ino(), inode);
}

#[test]
fn stale_workspace_enumeration_failure_retains_history() {
    let fx = Fx::new();
    git_repo(&fx.root);
    let missing = fx.root.join("missing metadata destination");
    std::fs::create_dir_all(&missing).unwrap();
    let history = format!(
        "{}{}",
        stale_workspace_block("local-historical"),
        stale_workspace_written_block(&missing)
    );
    std::fs::remove_dir(&missing).unwrap();
    let exclude = fx.root.join(".git/info/exclude");
    std::fs::write(&exclude, &history).unwrap();
    // A real failed git command produces uncertainty, not an empty live set.
    assert!(local::live_workspace_ids(&fx.content).is_none());
    crate::content_engine::git_exclude::replace_workspace_block_if_with_gc(
        &fx.root,
        &stale_workspace_id(&fx.root),
        Some(&[]),
        &["docs/a.md".into()],
        Some(&fx.root.canonicalize().unwrap()),
        || local::live_workspace_ids(&fx.content),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(exclude).unwrap(),
        format!("{history}{}", stale_workspace_written_block(&fx.root))
    );
}

#[test]
fn stale_workspace_retry_collects_and_preserves_concurrent_paths() {
    let fx = Fx::new();
    git_repo(&fx.root);
    let id = stale_workspace_id(&fx.root);
    let exclude = fx.root.join(".git/info/exclude");
    let own = stale_workspace_block(&id);
    std::fs::write(
        &exclude,
        format!("{}{own}", stale_workspace_block("local-stale")),
    )
    .unwrap();
    let result = local::write_block(&fx.root, &id, &[], &["docs/b.md".into()]).unwrap();
    assert_eq!(result, vec!["docs/a.md", "docs/b.md"]);
    let text = std::fs::read_to_string(exclude).unwrap();
    assert!(!text.contains("local-stale"));
    assert!(text.contains("/docs/a.md\n/docs/b.md\n"));
}

#[test]
fn stale_workspace_own_malformed_block_still_errors() {
    let fx = Fx::new();
    git_repo(&fx.root);
    let id = stale_workspace_id(&fx.root);
    let exclude = fx.root.join(".git/info/exclude");
    for broken in [
        format!("# >>> codesense managed workspace {id}\n/path\n"),
        format!("# <<< codesense managed workspace {id}\n"),
        format!("{}{}", stale_workspace_block(&id), stale_workspace_block(&id)),
        format!("# >>> codesense managed workspace {id}\nhand edited\n# <<< codesense managed workspace {id}\n"),
    ] {
        std::fs::write(&exclude, &broken).unwrap();
        assert!(local::write_block(&fx.root, &id, &[], &["docs/a.md".into()]).is_err());
        assert_eq!(std::fs::read_to_string(&exclude).unwrap(), broken);
    }
}

#[test]
fn stale_workspace_nested_linked_destinations_keep_both_blocks() {
    let fx = Fx::new();
    git_repo(&fx.root);
    let linked = stale_workspace_link(&fx, "gc-nested");
    let a = linked.join("destination a");
    let b = linked.join("destination b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let exclude = crate::content_engine::git_exclude::resolve_info_exclude_path(&a).unwrap();
    assert!(exclude.starts_with(fx.root.canonicalize().unwrap().join(".git/worktrees")));
    // This history is private to the linked worktree; listing-root GC must never run.
    let legacy = stale_workspace_block("local-private-history");
    std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    std::fs::write(&exclude, &legacy).unwrap();
    stale_workspace_write(&a);
    stale_workspace_write(&b);
    stale_workspace_write(&a);
    let text = std::fs::read_to_string(&exclude).unwrap();
    assert!(text.starts_with(&legacy));
    for root in [&a, &b] {
        let id = stale_workspace_id(root);
        assert!(text.contains(&format!(
            "# >>> codesense managed workspace {id}\n# root: {}\n",
            root.canonicalize().unwrap().display()
        )));
        assert_eq!(
            local::actual_block(root, &id).unwrap().unwrap(),
            vec!["docs/a.md"]
        );
    }
}

#[test]
fn stale_workspace_main_clone_subdirectory_metadata_survives_other_writes() {
    let fx = Fx::new();
    git_repo(&fx.root);
    let nested = fx.root.join("destination with spaces");
    std::fs::create_dir_all(&nested).unwrap();
    stale_workspace_write(&nested);
    let block = stale_workspace_written_block(&nested);
    let linked = stale_workspace_link(&fx, "gc-metadata-other");
    for root in [&fx.root, &linked] {
        stale_workspace_write(root);
        assert!(std::fs::read_to_string(fx.root.join(".git/info/exclude"))
            .unwrap()
            .contains(&block));
    }
    assert_eq!(
        crate::content_engine::git_exclude::workspace_block(&nested, &stale_workspace_id(&nested))
            .unwrap()
            .unwrap(),
        vec!["docs/a.md"]
    );
}

#[test]
fn stale_workspace_metadata_missing_root_is_collected() {
    let fx = Fx::new();
    git_repo(&fx.root);
    let missing = fx.root.join("removed destination with spaces");
    std::fs::create_dir_all(&missing).unwrap();
    stale_workspace_write(&missing);
    let block = stale_workspace_written_block(&missing);
    std::fs::remove_dir(&missing).unwrap();
    stale_workspace_write(&fx.root);
    let text = std::fs::read_to_string(fx.root.join(".git/info/exclude")).unwrap();
    assert!(!text.contains(&block));
    assert!(text.contains(&stale_workspace_written_block(&fx.root)));
}

#[test]
fn stale_workspace_unchanged_prepare_upgrades_and_collects_history() {
    let fx = Fx::new();
    git_repo(&fx.root);
    fx.team("docs/a.md", b"original content");
    let want = fx.want(&["docs"]);
    let first = fx.run(&want);
    assert_eq!(first.status, PrepareStatus::Ready);
    assert_eq!(first.written, vec!["docs/a.md"]);
    assert!(ManagedRecord::exists(
        fx.home.path(),
        &principal(),
        &crate::content_engine::context_lifecycle::local_identity(&fx.root)
    ));
    let exclude = fx.root.join(".git/info/exclude");
    std::fs::write(
        &exclude,
        format!(
            "{}{}",
            stale_workspace_block(&stale_workspace_id(&fx.root)),
            stale_workspace_block("local-historical")
        ),
    )
    .unwrap();

    let second = fx.run(&want);
    assert_eq!(second.status, PrepareStatus::Ready);
    assert!(second.written.is_empty());
    assert_eq!(second.unchanged, 1);
    assert_eq!(fx.read("docs/a.md"), b"original content");
    let expected = stale_workspace_written_block(&fx.root);
    assert_eq!(std::fs::read_to_string(&exclude).unwrap(), expected);
    use std::os::unix::fs::MetadataExt;
    let inode = std::fs::metadata(&exclude).unwrap().ino();
    let third = fx.run(&want);
    assert_eq!(third.status, PrepareStatus::Ready);
    assert!(third.written.is_empty());
    assert_eq!(std::fs::read_to_string(&exclude).unwrap(), expected);
    assert_eq!(std::fs::metadata(&exclude).unwrap().ino(), inode);
}

#[test]
fn stale_workspace_changed_content_same_paths_upgrades_and_collects_history() {
    let fx = Fx::new();
    git_repo(&fx.root);
    fx.team("docs/a.md", b"original content");
    let first = fx.run(&fx.want(&["docs"]));
    assert_eq!(first.status, PrepareStatus::Ready);
    assert_eq!(first.written, vec!["docs/a.md"]);
    let exclude = fx.root.join(".git/info/exclude");
    std::fs::write(
        &exclude,
        format!(
            "{}{}",
            stale_workspace_block(&stale_workspace_id(&fx.root)),
            stale_workspace_block("local-historical")
        ),
    )
    .unwrap();
    fx.team(
        "docs/a.md",
        b"updated content with the same destination path",
    );

    let second = fx.run(&fx.want(&["docs"]));
    assert_eq!(second.status, PrepareStatus::Ready);
    assert_eq!(second.written, vec!["docs/a.md"]);
    assert_eq!(
        fx.read("docs/a.md"),
        b"updated content with the same destination path"
    );
    assert_eq!(
        std::fs::read_to_string(&exclude).unwrap(),
        stale_workspace_written_block(&fx.root)
    );
}

#[cfg(unix)]
#[test]
fn stale_workspace_metadata_permission_error_retains_blocks() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let fx = Fx::new();
    git_repo(&fx.root);
    let parent = fx._dirs.path().join("restricted metadata roots");
    let roots = [parent.join("destination a"), parent.join("destination b")];
    for root in &roots {
        std::fs::create_dir_all(root).unwrap();
    }
    let protected: String = roots
        .iter()
        .map(|root| stale_workspace_written_block(root))
        .collect();
    let history = format!("{protected}{}", stale_workspace_block("local-stale"));
    let exclude = fx.root.join(".git/info/exclude");
    std::fs::write(&exclude, &history).unwrap();
    let permissions = std::fs::metadata(&parent).unwrap().permissions();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0)).unwrap();
    let probe = std::fs::metadata(&roots[0]);
    let result = local::write_block(
        &fx.root,
        &stale_workspace_id(&fx.root),
        &[],
        &["docs/a.md".into()],
    );
    std::fs::set_permissions(&parent, permissions).unwrap();

    assert_eq!(
        probe.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    result.unwrap();
    assert_eq!(
        std::fs::read_to_string(&exclude).unwrap(),
        format!("{protected}{}", stale_workspace_written_block(&fx.root))
    );
}

#[cfg(unix)]
#[test]
fn stale_workspace_legacy_permission_error_skips_gc() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let fx = Fx::new();
    git_repo(&fx.root);
    let parent = fx._dirs.path().join("restricted listed root");
    std::fs::create_dir_all(&parent).unwrap();
    let linked = parent.join("worktree");
    git(
        &fx.root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "gc-legacy-permission",
            linked.to_str().unwrap(),
        ],
    );
    let history = format!(
        "{}{}",
        stale_workspace_block(&stale_workspace_id(&linked)),
        stale_workspace_block("local-stale")
    );
    let exclude = fx.root.join(".git/info/exclude");
    std::fs::write(&exclude, &history).unwrap();
    let permissions = std::fs::metadata(&parent).unwrap().permissions();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0)).unwrap();
    let probe = std::fs::metadata(&linked);
    let listing =
        crate::commands::worktree::run_git(&["worktree", "list", "--porcelain", "-z"], &fx.root);
    let result = local::write_block(
        &fx.root,
        &stale_workspace_id(&fx.root),
        &[],
        &["docs/a.md".into()],
    );
    std::fs::set_permissions(&parent, permissions).unwrap();

    assert_eq!(
        probe.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(listing.unwrap().contains("gc-legacy-permission"));
    result.unwrap();
    assert_eq!(
        std::fs::read_to_string(&exclude).unwrap(),
        format!("{history}{}", stale_workspace_written_block(&fx.root))
    );
}
