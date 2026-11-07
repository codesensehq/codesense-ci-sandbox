// SPDX-License-Identifier: MIT
//! SW-XS-8 acceptance tests: explicit hydrates run on the Step 2 engine
//! (locally here; on a machine in `terminal/session_server/manifest_wire_tests.rs`).
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::super::*;
use super::*;
use crate::agents::resolve::tests::{alice, team_content, ORG, TEAM};

pub(crate) struct Fx {
    pub home: tempfile::TempDir,
    pub team: PathBuf,
    _dirs: tempfile::TempDir,
    pub root: PathBuf,
}

pub(crate) fn write(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
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

/// Every file under `root` with its bytes (team content's state).
pub(crate) fn snapshot(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| !e.path().components().any(|c| c.as_os_str() == ".git"))
        .map(|e| {
            (
                e.path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

impl Fx {
    pub(crate) fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let team = team_content(home.path(), &alice());
        let dirs = tempfile::tempdir().unwrap();
        let root = dirs.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        write(&root, "README.md", b"code\n");
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "init"]);
        Self {
            home,
            team,
            _dirs: dirs,
            root: root.canonicalize().unwrap(),
        }
    }
    pub(crate) fn target(&self) -> ManifestTarget {
        ManifestTarget::team(ORG, TEAM)
    }
    pub(crate) fn want(&self, paths: &[&str], sets: &[&str]) -> WantSet {
        let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        want_for_selection(
            self.home.path(),
            &alice(),
            &self.target(),
            &strings(paths),
            &strings(sets),
        )
        .unwrap()
    }
    pub(crate) fn hydrate_with(
        &self,
        want: &WantSet,
        prune: bool,
        overwrite_modified: bool,
        cancel: Option<&dyn Fn() -> bool>,
        progress: &dyn Fn(&PrepareProgress),
    ) -> PrepareReport {
        let legacy = || Ok(Vec::new());
        hydrate(Hydrate {
            home: self.home.path(),
            principal: &alice(),
            destination: Destination::Local { root: &self.root },
            want,
            operation: "op-1",
            workspace: "ws".into(),
            agent: "Barb".into(),
            prune,
            overwrite_modified,
            progress,
            validate: &|| Ok(()),
            cancel,
            legacy: &legacy,
        })
        .unwrap()
    }
    pub(crate) fn hydrate(&self, want: &WantSet) -> PrepareReport {
        self.hydrate_with(want, false, false, None, &|_| {})
    }
    pub(crate) fn read(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root.join(path)).ok()
    }
    pub(crate) fn record(&self) -> ManagedRecord {
        let identity = crate::content_engine::context_lifecycle::local_identity(&self.root);
        ManagedRecord::load(self.home.path(), &alice(), &identity)
    }
}

/// AC3: `hydrate_worktree {sets:[".claude/skills"]}` after a skill was
/// edited in team content transfers only that file.
#[test]
fn hydrate_sets_after_a_team_skill_edit_transfers_only_that_file() {
    let fx = Fx::new();
    let want = fx.want(&[], &[".claude/skills"]);
    let first = fx.hydrate(&want);
    assert_eq!(first.status, PrepareStatus::Ready);
    assert_eq!(first.written.len(), 2, "{first:?}");
    write(
        &fx.team,
        ".claude/skills/bmad-build/SKILL.md",
        b"# Build v2\n",
    );
    std::thread::sleep(std::time::Duration::from_millis(30));
    let again = fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    assert_eq!(again.written, vec![".claude/skills/bmad-build/SKILL.md"]);
    assert_eq!(
        fx.read(".claude/skills/bmad-build/SKILL.md").unwrap(),
        b"# Build v2\n"
    );
}

/// X7 + §11.2: a Step 3 add enters the managed record (origin
/// `step3:<operationId>`) and the workspace's exclude block in the same
/// operation, so `git status --porcelain` stays empty.
#[test]
fn a_step_three_add_is_managed_and_never_shows_in_git_status() {
    let fx = Fx::new();
    let report = fx.hydrate(&fx.want(&["CLAUDE.md"], &[".claude/commands"]));
    assert_eq!(report.written.len(), 2);
    let record = fx.record();
    for path in ["CLAUDE.md", ".claude/commands/bmad-help.md"] {
        let entry = &record.entries[path];
        assert_eq!(entry.origin, "step3:op-1");
        assert_eq!(entry.source_layer, format!("team:{ORG}/{TEAM}"));
        assert_eq!(entry.source.as_deref(), Some(fx.team.join(path).as_path()));
    }
    assert_eq!(
        git(
            &fx.root,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        ""
    );
    // An explicit hydrate never decides whether a launch is ready (X5).
    assert_eq!(record.last_status, None);
}

/// §11.1: Step 3 is additive; only `prune:true` removes, only unmodified
/// managed files no longer wanted, and (review r1 M2) only under what the
/// call names: pruning `.claude/hooks` never touches `.claude/commands`.
#[test]
fn hydrate_is_additive_and_a_prune_stays_in_its_scope() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/commands"]));
    fx.hydrate(&fx.want(&[], &[".claude/hooks"]));
    assert!(fx.read(".claude/commands/bmad-help.md").is_some());
    assert!(fx.read(".claude/hooks/team-hook.sh").is_some());
    // Without prune, a narrower hydrate removes nothing.
    let additive = fx.hydrate(&fx.want(&[], &[".claude/hooks"]));
    assert!(additive.removed.is_empty());
    // A prune of `.claude/hooks` leaves every other managed path alone…
    let scoped = fx.hydrate_with(
        &fx.want(&[], &[".claude/hooks"]),
        true,
        false,
        None,
        &|_| {},
    );
    assert!(scoped.removed.is_empty(), "{scoped:?}");
    assert!(fx.read(".claude/commands/bmad-help.md").is_some());
    // …and removes what team content no longer has under `.claude/hooks`.
    write(&fx.team, ".claude/hooks/extra.sh", b"echo extra\n");
    fx.hydrate(&fx.want(&[], &[".claude/hooks"]));
    assert!(fx.read(".claude/hooks/extra.sh").is_some());
    std::fs::remove_file(fx.team.join(".claude/hooks/extra.sh")).unwrap();
    let pruned = fx.hydrate_with(
        &fx.want(&[], &[".claude/hooks"]),
        true,
        false,
        None,
        &|_| {},
    );
    assert_eq!(pruned.removed, vec![".claude/hooks/extra.sh"]);
    assert!(fx.read(".claude/hooks/extra.sh").is_none());
    assert!(fx.read(".claude/hooks/team-hook.sh").is_some());
    assert!(fx.read(".claude/commands/bmad-help.md").is_some());
    assert_eq!(
        git(
            &fx.root,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        "",
        "the block was trimmed and still hides what stays"
    );
}

/// §11.2 / §11.15: `overwriteModified:true` is the explicit recovery for a
/// modified managed file (and an unmanaged one); without it the edit stays.
#[test]
fn overwrite_modified_restores_the_team_version() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&["CLAUDE.md"], &[]));
    write(&fx.root, "CLAUDE.md", b"my edits");
    write(&fx.root, ".claude/commands/bmad-help.md", b"not ours");
    let kept = fx.hydrate(&fx.want(&["CLAUDE.md"], &[".claude/commands"]));
    assert_eq!(kept.kept_modified, vec!["CLAUDE.md"]);
    assert_eq!(kept.kept_unmanaged, vec![".claude/commands/bmad-help.md"]);
    assert_eq!(fx.read("CLAUDE.md").unwrap(), b"my edits");
    let restored = fx.hydrate_with(
        &fx.want(&["CLAUDE.md"], &[".claude/commands"]),
        false,
        true,
        None,
        &|_| {},
    );
    assert_eq!(restored.status, PrepareStatus::Ready);
    assert_eq!(fx.read("CLAUDE.md").unwrap(), b"# Team\n");
    assert_eq!(
        fx.read(".claude/commands/bmad-help.md").unwrap(),
        b"Help.\n"
    );
}

/// X6: a hydrate never touches team content, whatever the workspace holds.
#[test]
fn hydrate_leaves_team_content_byte_identical_after_a_worktree_edit() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[]));
    write(&fx.root, "CLAUDE.md", b"edited in the worktree");
    write(
        &fx.root,
        ".claude/skills/new/SKILL.md",
        b"new in the worktree",
    );
    let org = fx.team.parent().unwrap().parent().unwrap().to_path_buf();
    let before = snapshot(&org);
    // The scope-less (legacy) form and a selected form.
    fx.hydrate(&fx.want(&[], &[]));
    fx.hydrate(&fx.want(&["CLAUDE.md"], &[".claude/skills"]));
    assert_eq!(snapshot(&org), before);
}

/// The scope-less form is the runtime defaults (claude ∪ codex).
#[test]
fn a_scope_less_hydrate_wants_the_runtime_defaults() {
    let fx = Fx::new();
    let dests: Vec<String> = fx
        .want(&[], &[])
        .files
        .into_iter()
        .map(|f| f.dest)
        .collect();
    for path in [
        "CLAUDE.md",
        ".claude/skills/bmad-build/SKILL.md",
        ".claude/commands/bmad-help.md",
        ".claude/hooks/team-hook.sh",
        ".agents/skills/review/SKILL.md",
    ] {
        assert!(dests.contains(&path.to_string()), "{path} in {dests:?}");
    }
    assert!(!dests.contains(&".mcp.json".to_string()));
    assert!(selection_entries(&[".git/config".into()], &[]).is_err());
    assert!(selection_entries(&["../x".into()], &[]).is_err());
    assert!(selection_entries(&[".mcp.json".into()], &[]).is_err());
}

/// AC5 (locally): cancelled mid-transfer, no partial file is visible in the
/// workspace — no staged copy, no half-written file — and the result is
/// `cancelled`, recording exactly what landed (nothing here).
#[test]
fn a_cancelled_hydrate_leaves_no_partial_file_and_reports_cancelled() {
    let fx = Fx::new();
    let want = fx.want(&[], &[".claude/skills"]);
    let events: Arc<Mutex<Vec<PrepareProgress>>> = Arc::default();
    let transfers = AtomicUsize::new(0);
    let cancel = || transfers.load(Ordering::SeqCst) >= 1;
    let sink = |p: &PrepareProgress| {
        if p.phase == PreparePhase::Transfer && p.files >= 1 {
            transfers.fetch_add(1, Ordering::SeqCst);
        }
        events.lock().unwrap().push(p.clone());
    };
    let report = fx.hydrate_with(&want, false, false, Some(&cancel), &sink);
    assert_eq!(report.status, PrepareStatus::Cancelled);
    assert!(report.written.is_empty(), "{report:?}");
    let leftovers: Vec<String> = walkdir::WalkDir::new(&fx.root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.contains(".claude"))
        .collect();
    assert!(leftovers.is_empty(), "partial files: {leftovers:?}");
    assert!(fx.record().entries.is_empty());
    assert_eq!(
        git(
            &fx.root,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        ""
    );
    let events = events.lock().unwrap();
    assert_eq!(events.last().unwrap().phase, PreparePhase::Cancelled);
}

/// §11.14 progress: the backend event sequence of one explicit operation —
/// every event names its operationId, it ends in exactly one terminal state.
#[test]
fn an_explicit_hydrate_emits_an_operation_event_sequence() {
    let fx = Fx::new();
    let events: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let sink = |p: &PrepareProgress| {
        events
            .lock()
            .unwrap()
            .push(serde_json::to_value(p).unwrap())
    };
    fx.hydrate_with(
        &fx.want(&[], &[".claude/skills"]),
        false,
        false,
        None,
        &sink,
    );
    let events = events.lock().unwrap();
    assert!(
        events.iter().all(|e| e["operationId"] == "op-1"),
        "{events:?}"
    );
    let phases: Vec<&str> = events
        .iter()
        .map(|e| e["phase"].as_str().unwrap())
        .collect();
    assert_eq!(phases.first(), Some(&"diff"));
    assert_eq!(phases.last(), Some(&"ready"));
    let terminal = phases
        .iter()
        .filter(|p| ["ready", "failed", "cancelled"].contains(p))
        .count();
    assert_eq!(terminal, 1);
    assert!(phases.contains(&"transfer") && phases.contains(&"apply"));
}

/// `hydrate_story`'s want set is the closure's files at their content paths;
/// a file outside every tier is counted, not copied.
#[test]
fn story_files_are_wanted_at_their_content_paths() {
    let fx = Fx::new();
    write(&fx.team, "_bmad-output/epic/story.md", b"story");
    write(&fx.team, "_bmad-output/epic/sub/notes.md", b"notes");
    let stray = fx.home.path().join("elsewhere.md");
    std::fs::write(&stray, b"x").unwrap();
    let (want, outside) = want_for_files(
        fx.home.path(),
        &alice(),
        &fx.target(),
        &[
            fx.team.join("_bmad-output/epic/story.md"),
            fx.team.join("_bmad-output/epic/sub/notes.md"),
            stray,
        ],
    );
    assert_eq!(outside, 1);
    let dests: Vec<&str> = want.files.iter().map(|f| f.dest.as_str()).collect();
    assert_eq!(
        dests,
        vec![
            "_bmad-output/epic/story.md",
            "_bmad-output/epic/sub/notes.md"
        ]
    );
    assert!(want
        .files
        .iter()
        .all(|f| f.layer == format!("team:{ORG}/{TEAM}")));
    let report = fx.hydrate(&want);
    assert_eq!(report.written.len(), 2);
    assert_eq!(
        git(
            &fx.root,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        ""
    );
}

/// AC4: `move_content_file` (and `delete_content_file`) in team content
/// change no worktree file: nothing fans out. The workspace sees the move
/// only when an agent hydrates.
#[test]
fn moving_team_content_changes_no_worktree_file_until_a_hydrate() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    let before = snapshot(&fx.root);
    crate::install::story_linker::apply_structural_source_op(
        &crate::install::story_linker::StructuralChange::Rename {
            from: fx.team.join(".claude/skills/bmad-build"),
            to: fx.team.join(".claude/skills/bmad-make"),
            is_dir: true,
        },
        None,
        None,
    )
    .unwrap();
    crate::install::story_linker::apply_structural_source_op(
        &crate::install::story_linker::StructuralChange::Delete {
            source_path: fx.team.join("CLAUDE.md"),
            is_dir: false,
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(snapshot(&fx.root), before, "no worktree file changed");
    let report = fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    assert!(fx.read(".claude/skills/bmad-make/SKILL.md").is_some());
    assert_eq!(report.written.len(), 2);
    // Additive: the old copies stay until a prune.
    assert!(fx.read(".claude/skills/bmad-build/SKILL.md").is_some());
}

/// The explicit-operation event contract the frontend consumes (XS-10): a
/// golden fixture of one hydrate's events (each carrying its operationId,
/// ending `ready`) and one cancelled hydrate (ending `cancelled`), checked in
/// at `src/tauri/fixtures/explicit-operation-progress.golden.json` and read by
/// the TypeScript contract test. Regenerate with `UPDATE_GOLDEN=1`.
#[test]
fn explicit_operation_progress_golden_fixture() {
    let fx = Fx::new();
    let events: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let sink = |p: &PrepareProgress| {
        let mut payload = serde_json::to_value(p).unwrap();
        if let Some(summary) = payload.get_mut("summary") {
            summary["elapsedMs"] = 0.into();
        }
        if let Some(note) = payload
            .get("note")
            .and_then(|n| n.as_str())
            .map(str::to_string)
        {
            let note = regex::Regex::new(r"in [0-9.]+ s")
                .unwrap()
                .replace(&note, "in 0.0 s");
            payload["note"] = note.into_owned().into();
        }
        events.lock().unwrap().push(payload);
    };
    fx.hydrate_with(
        &fx.want(&[], &[".claude/commands"]),
        false,
        false,
        None,
        &sink,
    );
    let ready: Vec<serde_json::Value> = std::mem::take(&mut *events.lock().unwrap());
    let cancel = || true;
    fx.hydrate_with(
        &fx.want(&[], &[".claude/skills"]),
        false,
        false,
        Some(&cancel),
        &sink,
    );
    let cancelled: Vec<serde_json::Value> = std::mem::take(&mut *events.lock().unwrap());
    let golden = serde_json::json!({
        "event": PREPARE_PROGRESS_EVENT,
        "ready": ready,
        "cancelled": cancelled,
    });
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../src/tauri/fixtures/explicit-operation-progress.golden.json");
    let rendered = serde_json::to_string_pretty(&golden).unwrap() + "\n";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &rendered).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap_or_default(),
        rendered,
        "the explicit-operation event contract changed: rerun with UPDATE_GOLDEN=1 and update the TS type"
    );
}

/// Review r1 (8a R1): past 32 sets the manifest prefixes fold to their
/// top-level folders; the prune still stays inside the sets the call named.
#[test]
fn a_prune_over_many_sets_stays_in_those_sets() {
    let fx = Fx::new();
    let sets: Vec<String> = (0..33).map(|i| format!(".claude/skills/s{i:02}")).collect();
    for set in &sets {
        write(&fx.team, &format!("{set}/SKILL.md"), b"# skill\n");
    }
    fx.hydrate(&fx.want(&[], &[".claude/commands"]));
    assert!(fx.read(".claude/commands/bmad-help.md").is_some());
    let named: Vec<&str> = sets.iter().map(String::as_str).collect();
    let want = fx.want(&[], &named);
    assert!(
        want.prefixes.iter().any(|p| p == ".claude"),
        "the manifest prefixes folded: {:?}",
        want.prefixes
    );
    fx.hydrate(&want);
    let pruned = fx.hydrate_with(&want, true, false, None, &|_| {});
    assert!(pruned.removed.is_empty(), "{pruned:?}");
    assert!(fx.read(".claude/commands/bmad-help.md").is_some());
}

#[test]
fn chosen_context_hydrates_uncommitted_files_and_cross_context_publish_conflicts() {
    let fx = Fx::new();
    write(&fx.team, "docs/chosen.md", b"base\n");
    let row = fx.context("chosen");
    let content = row.launch_cwd.clone();
    write(&content, "docs/chosen.md", b"worktree draft\n");
    write(&fx.team, "docs/chosen.md", b"canonical changed\n");
    let mut target = fx.target();
    target.context_root = Some(crate::agents::manifest_resolve::ContextRoot {
        id: row.id.clone(),
        root: row.root.clone(),
    });
    let want = want_for_selection(
        fx.home.path(),
        &alice(),
        &target,
        &["docs/chosen.md".into()],
        &[],
    )
    .unwrap();
    assert_eq!(fx.hydrate(&want).status, PrepareStatus::Ready);
    assert_eq!(fx.read("docs/chosen.md").unwrap(), b"worktree draft\n");
    assert_eq!(
        fx.record().entries["docs/chosen.md"]
            .source_context
            .as_deref(),
        Some(row.id.as_str())
    );
    write(&fx.root, "docs/chosen.md", b"workspace edit\n");
    let preview = |target: &ManifestTarget| {
        publish::preview(&publish::PublishRequest {
            home: fx.home.path(),
            principal: &alice(),
            destination: Destination::Local { root: &fx.root },
            target,
            paths: &[],
            sets: &[],
            to: None,
            expect: &Default::default(),
            include_deletes: false,
            legacy: None,
            operation: "test",
            workspace: "test".into(),
            agent: "test".into(),
            progress: &|_| {},
            validate: &|| Ok(()),
            cancel: None,
        })
        .unwrap()
    };
    assert!(preview(&fx.target())
        .conflicts
        .iter()
        .any(|e| e.path == "docs/chosen.md"));
    assert!(preview(&target).conflicts.is_empty());
}

#[test]
fn identical_rehydrate_records_the_selected_context_and_counts_against_it() {
    let fx = Fx::new();
    write(&fx.team, "docs/shared.md", b"base\n");
    let a = fx.context("source-a");
    let b = fx.context("source-b");
    let mut target = fx.target();
    for row in [&a, &b] {
        target.context_root = Some(crate::agents::manifest_resolve::ContextRoot {
            id: row.id.clone(),
            root: row.root.clone(),
        });
        let want = want_for_selection(
            fx.home.path(),
            &alice(),
            &target,
            &["docs/shared.md".into()],
            &[],
        )
        .unwrap();
        let report = fx.hydrate(&want);
        assert_eq!(report.status, PrepareStatus::Ready);
        if row.id == b.id {
            assert!(report.written.is_empty(), "identical bytes need no write");
            assert_eq!(report.unchanged, 1);
        }
    }
    let record = fx.record();
    let entry = &record.entries["docs/shared.md"];
    let source = b.launch_cwd.join("docs/shared.md");
    assert_eq!(entry.source.as_ref(), Some(&source));
    assert_eq!(entry.source_context.as_deref(), Some(b.id.as_str()));
    assert_eq!(
        entry.source_stat,
        Some(StatKey::of(&std::fs::metadata(&source).unwrap()))
    );
    write(
        &a.launch_cwd,
        "docs/shared.md",
        b"A changed independently\n",
    );
    write(&fx.root, "docs/shared.md", b"workspace edit\n");
    let edits = edits::count(&edits::CountRequest {
        home: fx.home.path(),
        principal: &alice(),
        destination: Destination::Local { root: &fx.root },
        target: &fx.target(),
        legacy: None,
    });
    assert_eq!(edits.state, edits::EditsState::Checked);
    assert_eq!(edits.contexts, vec![b.id]);
    assert_eq!(edits.counts.changed, 1);
    assert_eq!(edits.counts.conflicts, 0);
    assert_eq!(edits.files[0].team_path, source);
}

impl Fx {
    pub(crate) fn context(
        &self,
        name: &str,
    ) -> crate::install::worktree_index::ContextWorktreeRecord {
        use crate::install::worktree_index::{self, ContextWorktreeRecord};
        let org = crate::context_repo::path_to_org_root(self.home.path(), &alice(), ORG).unwrap();
        if git2::Repository::open(&org).is_err() {
            git(&org, &["init", "-q", "-b", "main"]);
            git(&org, &["add", "."]);
            git(&org, &["commit", "-qm", "context baseline"]);
        }
        let base = git(&org, &["rev-parse", "main"]).trim().to_string();
        let root = self._dirs.path().join(name);
        let branch = format!("{TEAM}/{name}");
        git(
            &org,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                root.to_str().unwrap(),
                "main",
            ],
        );
        let root = root.canonicalize().unwrap();
        let row = ContextWorktreeRecord {
            owner: Some(worktree_index::ContextWorktreeOwner::Person),
            schema_version: worktree_index::SCHEMA_VERSION,
            kind: "context".into(),
            id: format!("cw_{name}"),
            org: ORG.into(),
            team: TEAM.into(),
            name: name.into(),
            branch,
            base_sha: base,
            launch_cwd: root.join(TEAM).join("content"),
            root,
            created_at: 1,
            last_used_at: 1,
            extras: Default::default(),
        };
        worktree_index::write_context_record(self.home.path(), &alice(), &row).unwrap();
        row
    }
}

#[test]
fn context_tools_read_and_write_files_without_moving_any_git_ref() {
    use crate::agents::prepare::publish::{self, PublishRequest};
    let fx = Fx::new();
    write(&fx.team, "docs/edit.md", b"base\n");
    write(
        &fx.team,
        "stories/task.md",
        b"---\ntype: story\nsource_repo: acme/api\nstory_id: TASK\n---\noriginal story\n",
    );
    let row = fx.context("sprint");
    let canonical = crate::context_repo::path_to_org_root(fx.home.path(), &alice(), ORG).unwrap();
    let before = git(&canonical, &["show-ref"]);
    let mut target = fx.target();
    target.repo = Some(crate::domain::SourceRepoSlug::new("acme", "api").unwrap());
    target.context_root = Some(crate::agents::manifest_resolve::ContextRoot {
        id: row.id.clone(),
        root: row.root.clone(),
    });
    write(
        &row.launch_cwd,
        "docs/edit.md",
        b"manager's uncommitted draft\n",
    );
    let want = want_for_selection(
        fx.home.path(),
        &alice(),
        &target,
        &["docs/edit.md".into()],
        &[],
    )
    .unwrap();
    assert_eq!(fx.hydrate(&want).status, PrepareStatus::Ready);
    write(&fx.root, "docs/edit.md", b"agent's edit\n");
    let count = edits::count(&edits::CountRequest {
        home: fx.home.path(),
        principal: &alice(),
        destination: Destination::Local { root: &fx.root },
        target: &fx.target(),
        legacy: None,
    });
    assert_eq!(count.counts.changed, 1);
    assert_eq!(count.contexts, vec![row.id.clone()]);
    let expect = Default::default();
    let request = PublishRequest {
        home: fx.home.path(),
        principal: &alice(),
        destination: Destination::Local { root: &fx.root },
        target: &target,
        paths: &[],
        sets: &[],
        to: None,
        include_deletes: false,
        expect: &expect,
        legacy: None,
        operation: "no-ref-movement",
        workspace: "workspace".into(),
        agent: "test".into(),
        progress: &|_| {},
        validate: &|| Ok(()),
        cancel: None,
    };
    assert_eq!(publish::preview(&request).unwrap().entries.len(), 1);
    assert_eq!(publish::publish(&request).unwrap().entries.len(), 1);
    assert_eq!(
        std::fs::read(row.launch_cwd.join("docs/edit.md")).unwrap(),
        b"agent's edit\n"
    );
    assert_eq!(
        std::fs::read(fx.team.join("docs/edit.md")).unwrap(),
        b"base\n"
    );
    write(
        &row.launch_cwd,
        "stories/task.md",
        b"---\ntype: story\nsource_repo: acme/api\nstory_id: TASK\n---\nmanager's story edit\n",
    );
    let ctx = crate::content_engine::ResolverContext {
        context_root: target.context_root.clone(),
        codesense_home: fx.home.path().into(),
        user_id: 1,
        org_slug: ORG.into(),
        team_name: Some(TEAM.into()),
    };
    let cache = crate::install::frontmatter_cache::FrontmatterCache::shared(
        crate::install::frontmatter_cache::FrontmatterCache::new(),
    );
    let story = crate::commands::route_to_context::remote_story_files(
        &ctx,
        target.repo.as_ref().unwrap(),
        "TASK",
        &cache,
        None,
    )
    .unwrap();
    let (want, outside) = want_for_files(fx.home.path(), &alice(), &target, &story.files);
    assert_eq!(outside, 0);
    assert_eq!(fx.hydrate(&want).status, PrepareStatus::Ready);
    assert!(String::from_utf8(fx.read("stories/task.md").unwrap())
        .unwrap()
        .contains("manager's story edit"));
    let _ = crate::commands::git_sync::context_worktree::status_impl(&row, vec![]).unwrap();
    let _ = crate::commands::git_sync::context_worktree::publish_set(&row.root, None).unwrap();
    let _ = crate::commands::git_sync::context_worktree::redline(
        &row,
        &format!("{TEAM}/content/docs/edit.md"),
        None,
    )
    .unwrap();
    assert_eq!(git(&canonical, &["show-ref"]), before);
    assert_eq!(git(&row.root, &["show-ref"]), before);
}

pub(crate) fn hydrate_defaults(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    target: &crate::agents::ManifestTarget,
) -> (tempfile::TempDir, Vec<std::path::PathBuf>) {
    let workspace = tempfile::tempdir().unwrap();
    let legacy = || Ok(Vec::new());
    for sets in [vec![], vec!["skills".to_string()]] {
        let want = want_for_selection(home, principal, target, &[], &sets).unwrap();
        hydrate(Hydrate {
            home,
            principal,
            destination: crate::agents::prepare::Destination::Local {
                root: workspace.path(),
            },
            want: &want,
            operation: "seam",
            workspace: "seam".into(),
            agent: "seam".into(),
            prune: false,
            overwrite_modified: false,
            progress: &|_| {},
            validate: &|| Ok(()),
            cancel: None,
            legacy: &legacy,
        })
        .unwrap();
    }
    let paths = walkdir::WalkDir::new(workspace.path())
        .into_iter()
        .filter_map(Result::ok)
        .map(|e| e.path().to_path_buf())
        .collect();
    (workspace, paths)
}

#[test]
fn inherited_hydrate_selection_and_runtime_defaults_exclude_private_files() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    std::fs::remove_file(root.join("child/content/secret.md")).unwrap();
    std::fs::remove_file(root.join("child/content/shared.md")).unwrap();
    let target = ManifestTarget::team("acme/ctx", "child");
    let want = want_for_selection(
        home.path(),
        &alice(),
        &target,
        &["shared.md".into(), "secret.md".into()],
        &[],
    )
    .unwrap();
    assert!(want
        .files
        .iter()
        .any(|f| f.source == root.join("parent/content/shared.md")));
    assert!(want.files.iter().all(|f| !f.source.ends_with("secret.md")));
    let defaults = WantSet::for_runtime(
        home.path(),
        &alice(),
        codesense_pty_core::agent_cli::AgentCli::Claude,
        &target,
        false,
    )
    .unwrap();
    assert!(defaults
        .files
        .iter()
        .all(|f| !f.source.ends_with("grand/SKILL.md")));
    assert!(defaults
        .files
        .iter()
        .all(|f| f.source != root.join("parent/content/secret.md")));
    let workspace = tempfile::tempdir().unwrap();
    git(workspace.path(), &["init", "-q"]);
    let legacy = || Ok(Vec::new());
    for wanted in [&want, &defaults] {
        let report = hydrate(Hydrate {
            home: home.path(),
            principal: &alice(),
            destination: Destination::Local {
                root: workspace.path(),
            },
            want: wanted,
            operation: "inheritance-fixture",
            workspace: "fixture".into(),
            agent: "fixture".into(),
            prune: false,
            overwrite_modified: false,
            progress: &|_| {},
            validate: &|| Ok(()),
            cancel: None,
            legacy: &legacy,
        })
        .unwrap();
        assert_eq!(report.status, PrepareStatus::Ready, "{report:?}");
    }
    assert_eq!(
        std::fs::read(workspace.path().join("shared.md")).unwrap(),
        b"parent-shared.md-bytes"
    );
    assert!(!workspace.path().join("secret.md").exists());
    assert!(!workspace.path().join(".claude/skills/grand").exists());
}

#[test]
fn remote_want_set_and_arm_manifest_hash_change_when_a_path_is_unshared() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    std::fs::remove_file(root.join("child/content/shared.md")).unwrap();
    let target = ManifestTarget::team("acme/ctx", "child");
    let (before, outside) = want_for_files(
        home.path(),
        &alice(),
        &target,
        &[
            root.join("parent/content/shared.md"),
            root.join("parent/content/secret.md"),
            root.join("grand/content/secret.md"),
        ],
    );
    assert_eq!(outside, 2);
    assert_eq!(before.files.len(), 1);
    let first =
        want_for_selection(home.path(), &alice(), &target, &["shared.md".into()], &[]).unwrap();
    let mut agent = carrier(Vec::new());
    agent.startup = vec!["shared.md".into()];
    let reference = crate::agents::manifest::AgentRevisionRef {
        key: crate::agents::keys::AgentKey::new("acme/ctx", "child", "carrier"),
        revision: 1,
        definition_hash: crate::agents::manifest::content_hash(b"carrier"),
    };
    let manifest = || {
        crate::agents::manifest_resolve::resolve_manifest(
            home.path(),
            &alice(),
            &agent,
            &reference,
            target.clone(),
            Vec::new(),
            Vec::new(),
        )
    };
    let before_hash = manifest().manifest_hash;
    std::fs::write(root.join("parent/config/sharing.yml"), "share: []\n").unwrap();
    let after =
        want_for_selection(home.path(), &alice(), &target, &["shared.md".into()], &[]).unwrap();
    assert!(after.files.is_empty());
    assert!(!first.files.is_empty());
    assert_ne!(before_hash, manifest().manifest_hash);
}

#[cfg(unix)]
#[test]
fn story_hydration_rejects_a_child_directory_alias_to_private_parent_content() {
    use crate::install::frontmatter_cache::FrontmatterCache;
    use crate::install::hydration_closure::{HydrationPolicy, LIGHT};
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    let seed = root.join("child/content/epic/story.md");
    let hidden = root.join("parent/content/private");
    std::fs::create_dir_all(seed.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&hidden).unwrap();
    std::fs::write(&seed, "---\ntype: story\nsource_repo: acme/api\nstory_id: SEED\nteam: child\nsupporting_docs: [../linked/story.md]\n---\nchild bytes\n").unwrap();
    std::fs::write(hidden.join("story.md"), "---\ntype: story\nsource_repo: acme/api\nstory_id: PRIVATE\nteam: parent\n---\nparent private bytes\n").unwrap();
    std::os::unix::fs::symlink(
        "../../parent/content/private",
        root.join("child/content/linked"),
    )
    .unwrap();
    let cache = FrontmatterCache::new_shared();
    FrontmatterCache::rebuild_from_disk(&cache, &root);
    let ctx = crate::commands::content_engine::derive_resolver_context(
        home.path().to_path_buf(),
        1,
        "acme/ctx".into(),
        Some("child".into()),
    )
    .unwrap();
    let story = crate::commands::route_to_context::remote_story_files(
        &ctx,
        &crate::domain::SourceRepoSlug::parse_owner_repo("acme/api").unwrap(),
        "SEED",
        &cache,
        Some(HydrationPolicy {
            reference_depth: 1,
            ..LIGHT
        }),
    )
    .unwrap();
    assert_eq!(story.epic_dirs, vec![seed.parent().unwrap().to_path_buf()]);
    assert_eq!(story.files, vec![seed.clone()]);
    let target = ManifestTarget::team("acme/ctx", "child");
    let (want, outside) = want_for_files(home.path(), &alice(), &target, &story.files);
    assert_eq!(outside, 0);
    let alias = root.join("child/content/linked/story.md");
    let (denied, _) = want_for_files(home.path(), &alice(), &target, &[alias]);
    assert!(denied.files.is_empty());
    let workspace = tempfile::tempdir().unwrap();
    git(workspace.path(), &["init", "-q"]);
    let legacy = || Ok(Vec::new());
    let report = hydrate(Hydrate {
        home: home.path(),
        principal: &alice(),
        destination: Destination::Local {
            root: workspace.path(),
        },
        want: &want,
        operation: "alias",
        workspace: "alias".into(),
        agent: "alias".into(),
        prune: false,
        overwrite_modified: false,
        progress: &|_| {},
        validate: &|| Ok(()),
        cancel: None,
        legacy: &legacy,
    })
    .unwrap();
    assert_eq!(report.status, PrepareStatus::Ready);
    assert_eq!(
        std::fs::read(workspace.path().join("epic/story.md")).unwrap(),
        std::fs::read(seed).unwrap()
    );
    assert!(!workspace.path().join("linked").exists());
    assert!(!format!("{:?}", want.files).contains("PRIVATE"));
}

#[test]
fn narrowly_shared_nested_context_reaches_leaf_files_and_default_skills() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    let docs = root.join("parent/content/docs/sub/a.md");
    std::fs::create_dir_all(docs.parent().unwrap()).unwrap();
    std::fs::write(&docs, "shared nested text").unwrap();
    std::fs::write(root.join("parent/content/docs/secret.md"), "private text").unwrap();
    let target = ManifestTarget::team("acme/ctx", "child");
    for pattern in ["content/docs/sub/a.md", "content/docs/sub/**"] {
        std::fs::write(
            root.join("parent/config/sharing.yml"),
            format!("share: ['{pattern}', 'content/.claude/skills/parent/SKILL.md']\n"),
        )
        .unwrap();
        let want =
            want_for_selection(home.path(), &alice(), &target, &["docs".into()], &[]).unwrap();
        assert_eq!(want.files.len(), 1, "{pattern}: {:?}", want.files);
        assert_eq!(want.files[0].source, docs);
        assert_eq!(want.files[0].dest, "docs/sub/a.md");
        let skills = WantSet::for_runtime(
            home.path(),
            &alice(),
            codesense_pty_core::agent_cli::AgentCli::Claude,
            &target,
            false,
        )
        .unwrap();
        assert!(
            skills
                .files
                .iter()
                .any(|f| f.source == root.join("parent/content/.claude/skills/parent/SKILL.md")),
            "{:?}",
            skills.files
        );
        assert!(skills
            .files
            .iter()
            .all(|f| f.source != root.join("grand/content/.claude/skills/grand/SKILL.md")));
    }
}

#[test]
fn inherited_bulk_hydration_keeps_git_ignore_and_tracked_file_semantics() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    git(&root, &["init", "-q"]);
    std::fs::write(root.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(root.join(".git/info/exclude"), "local.md\n").unwrap();
    let docs = root.join("parent/content/docs");
    std::fs::create_dir_all(&docs).unwrap();
    for name in ["public.md", "generated.log", "tracked.log", "local.md"] {
        std::fs::write(docs.join(name), name).unwrap();
    }
    git(
        &root,
        &["add", "-f", "--", "parent/content/docs/tracked.log"],
    );
    std::fs::write(
        root.join("parent/config/sharing.yml"),
        "share: ['content/docs/**']\n",
    )
    .unwrap();
    for team in ["parent", "child"] {
        let target = ManifestTarget::team("acme/ctx", team);
        let want =
            want_for_selection(home.path(), &alice(), &target, &["docs".into()], &[]).unwrap();
        let paths: std::collections::BTreeSet<_> =
            want.files.iter().map(|f| f.dest.as_str()).collect();
        assert_eq!(
            paths,
            std::collections::BTreeSet::from(["docs/public.md", "docs/tracked.log"]),
            "{team}"
        );
    }
}

#[cfg(unix)]
#[test]
fn explicit_hydration_alias_owner_matrix_uses_only_admitted_real_sources() {
    let fixture = crate::context_repo::inheritance::alias_fixture();
    let target = ManifestTarget::team("acme/ctx", "child");
    let workspace = tempfile::tempdir().unwrap();
    git(workspace.path(), &["init", "-q"]);
    let legacy = || Ok(Vec::new());
    for case in &fixture.cases {
        let make_want = || {
            want_for_files(
                fixture.home.path(),
                &alice(),
                &target,
                &[case.content.clone()],
            )
        };
        let (want, outside) = make_want();
        assert_eq!(want.files.len(), usize::from(case.admitted), "{}", case.id);
        assert_eq!(outside, usize::from(!case.admitted), "{}", case.id);
        let report = hydrate(Hydrate {
            home: fixture.home.path(),
            principal: &alice(),
            destination: Destination::Local {
                root: workspace.path(),
            },
            want: &want,
            operation: &case.id,
            workspace: "matrix".into(),
            agent: "matrix".into(),
            prune: false,
            overwrite_modified: false,
            progress: &|_| {},
            validate: &|| Ok(()),
            cancel: None,
            legacy: &legacy,
        })
        .unwrap();
        assert_eq!(report.status, PrepareStatus::Ready, "{}", case.id);
        let dest = workspace.path().join(format!("{}.md", case.id));
        if case.admitted {
            assert_eq!(
                std::fs::read(dest).unwrap(),
                std::fs::read(&case.target_content).unwrap()
            );
            assert_eq!(
                want.files[0].source,
                case.target_content.canonicalize().unwrap()
            );
        } else {
            assert!(!dest.exists());
            std::fs::remove_file(&case.content).unwrap();
            let (missing, missing_outside) = make_want();
            assert_eq!(missing.files, want.files, "{}", case.id);
            assert_eq!(missing_outside, outside, "{}", case.id);
        }
    }
}

#[cfg(unix)]
#[test]
fn retained_story_hydration_intersects_real_checkout_shares_before_copying() {
    use crate::install::worktree_index::{self, ContextWorktreeRecord};
    let (home, canonical) = crate::context_repo::inheritance::test_fixture();
    let dirs = tempfile::tempdir().unwrap();
    let story = |id: &str| {
        format!("---\ntype: story\nsource_repo: acme/api\nstory_id: {id}\n---\n{id}_BYTES\n")
    };
    for id in ["shared", "secret", "source-denied", "selected-denied"] {
        write(
            &canonical,
            &format!("parent/content/{id}/story.md"),
            story(id).as_bytes(),
        );
    }
    write(
        &canonical,
        "sibling/content/epic/story.md",
        story("sibling").as_bytes(),
    );
    write(
        &canonical,
        "parent/config/sharing.yml",
        b"share: ['content/shared/**', 'content/selected-denied/**']\n",
    );
    git(&canonical, &["init", "-q", "-b", "main"]);
    git(&canonical, &["add", "."]);
    git(&canonical, &["commit", "-qm", "context baseline"]);
    let selected = dirs.path().join("retained");
    git(
        &canonical,
        &[
            "worktree",
            "add",
            "-b",
            "child/retained",
            selected.to_str().unwrap(),
            "main",
        ],
    );
    let selected = selected.canonicalize().unwrap();
    write(
        &selected,
        "parent/config/sharing.yml",
        b"share: ['content/shared/**', 'content/source-denied/**']\n",
    );
    let copied = dirs.path().join("unregistered-copy");
    git(
        dirs.path(),
        &[
            "clone",
            "-q",
            "--no-hardlinks",
            canonical.to_str().unwrap(),
            copied.to_str().unwrap(),
        ],
    );
    let moved = dirs.path().join("moved-copy");
    std::fs::rename(copied, &moved).unwrap();
    let unrelated = dirs.path().join("unrelated");
    write(
        &unrelated,
        "org.yml",
        b"slug: other/ctx\ndisplay_name: Other\ncreated_at: '2026-01-01T00:00:00Z'\n",
    );
    write(&unrelated, "teams.yml", b"teams:\n  - name: parent\n");
    write(
        &unrelated,
        "parent/content/epic/story.md",
        story("unrelated").as_bytes(),
    );
    let row = ContextWorktreeRecord {
        owner: Some(worktree_index::ContextWorktreeOwner::Person),
        schema_version: worktree_index::SCHEMA_VERSION,
        kind: "context".into(),
        id: "cw_retained".into(),
        org: "acme/ctx".into(),
        team: "child".into(),
        name: "retained".into(),
        branch: "child/retained".into(),
        base_sha: git(&canonical, &["rev-parse", "main"]).trim().into(),
        launch_cwd: selected.join("child/content"),
        root: selected.clone(),
        created_at: 1,
        last_used_at: 1,
        extras: Default::default(),
    };
    worktree_index::write_context_record(home.path(), &alice(), &row).unwrap();
    assert_eq!(
        worktree_index::resolve_context_record(
            home.path(),
            &alice(),
            "acme/ctx",
            Some("child"),
            &row.id
        )
        .unwrap(),
        row
    );
    let mut target = ManifestTarget::team("acme/ctx", "child");
    target.repo = Some(crate::domain::SourceRepoSlug::new("acme", "api").unwrap());
    target.context_root = Some(crate::agents::manifest_resolve::ContextRoot {
        id: row.id.clone(),
        root: selected.clone(),
    });
    let ctx = crate::content_engine::ResolverContext {
        codesense_home: home.path().into(),
        user_id: 1,
        org_slug: "acme/ctx".into(),
        team_name: Some("child".into()),
        context_root: target.context_root.clone(),
    };
    let mut edges = Vec::new();
    let mut denied_aliases = Vec::new();
    for (prefix, source) in [("canonical", &canonical), ("moved", &moved)] {
        for id in ["shared", "secret", "source-denied", "selected-denied"] {
            let alias = selected.join(format!("child/content/{prefix}-{id}"));
            std::os::unix::fs::symlink(source.join(format!("parent/content/{id}")), &alias)
                .unwrap();
            edges.push(format!("  - ../{prefix}-{id}/story.md"));
            if id != "shared" {
                denied_aliases.push(alias);
            }
        }
    }
    for (id, source) in [
        ("sibling", canonical.join("sibling/content/epic")),
        ("unrelated", unrelated.join("parent/content/epic")),
    ] {
        std::os::unix::fs::symlink(source, selected.join(format!("child/content/{id}"))).unwrap();
        edges.push(format!("  - ../{id}/story.md"));
    }
    write(&selected, "child/content/seed/story.md", format!("---\ntype: story\nsource_repo: acme/api\nstory_id: SEED\nsupporting_docs:\n{}\n---\nSEED_BYTES\n", edges.join("\n")).as_bytes());
    let remote = || {
        let cache = crate::install::frontmatter_cache::FrontmatterCache::shared(
            crate::install::frontmatter_cache::FrontmatterCache::new(),
        );
        crate::commands::route_to_context::remote_story_files(
            &ctx,
            target.repo.as_ref().unwrap(),
            "SEED",
            &cache,
            Some(crate::install::hydration_closure::HydrationPolicy {
                reference_depth: 1,
                ancestry_levels: 0,
                sibling_breadth: false,
                max_members: 64,
            }),
        )
        .unwrap()
    };
    let files = remote().files;
    assert_eq!(files.len(), 5, "{files:?}");
    let (want, outside) = want_for_files(home.path(), &alice(), &target, &files);
    assert_eq!((want.files.len(), outside), (5, 0));
    let workspace = dirs.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    git(&workspace, &["init", "-q"]);
    let legacy = || Ok(Vec::new());
    let hydrate_want = |want: &WantSet, operation: &str| {
        hydrate(Hydrate {
            home: home.path(),
            principal: &alice(),
            destination: Destination::Local { root: &workspace },
            want,
            operation,
            workspace: "retained".into(),
            agent: "test".into(),
            prune: false,
            overwrite_modified: false,
            progress: &|_| {},
            validate: &|| Ok(()),
            cancel: None,
            legacy: &legacy,
        })
        .unwrap()
    };
    let report = hydrate_want(&want, "retained");
    assert_eq!(report.status, PrepareStatus::Ready);
    let copied = snapshot(&workspace);
    assert_eq!(copied.len(), 5, "{copied:?}");
    for bytes in copied.values() {
        let text = String::from_utf8_lossy(bytes);
        assert!(
            !text.contains("secret_BYTES")
                && !text.contains("source-denied_BYTES")
                && !text.contains("selected-denied_BYTES")
        );
    }
    for alias in denied_aliases {
        std::fs::remove_file(alias).unwrap();
    }
    assert_eq!(
        remote().files,
        files,
        "denied and missing must have identical files and counts"
    );
    let (missing, missing_outside) =
        want_for_files(home.path(), &alice(), &target, &remote().files);
    assert_eq!(missing.files, want.files);
    assert_eq!(missing_outside, outside);
    write(
        &selected,
        "teams.yml",
        b"teams:\n  - name: parent\n    parent_team: grand\n  - name: grand\n  - name: sibling\n",
    );
    write(&selected, "child/content/seed/story.md", b"---\ntype: story\nsource_repo: acme/api\nstory_id: SEED\nsupporting_docs:\n  - ../../../parent/content/secret/story.md\n---\nSEED_BYTES\n");
    let unknown = remote().files;
    assert_eq!(unknown, vec![selected.join("child/content/seed/story.md")], "a valid older selected registry cannot prove another same-org team is outside the reader's ancestry");
    let (unknown_want, unknown_outside) = want_for_files(home.path(), &alice(), &target, &unknown);
    assert_eq!((unknown_want.files.len(), unknown_outside), (1, 0));
    std::fs::remove_file(selected.join("parent/content/secret/story.md")).unwrap();
    assert_eq!(
        remote().files,
        unknown,
        "unknown selected reader: denied and missing have the same scoped files and counts"
    );
    let (missing_want, missing_outside) =
        want_for_files(home.path(), &alice(), &target, &remote().files);
    assert_eq!(missing_want.files, unknown_want.files);
    assert_eq!(missing_outside, unknown_outside);

    write(&selected, "teams.yml", b"teams:\n  - name: child\n  - name: parent\n    parent_team: grand\n  - name: grand\n  - name: sibling\n");
    std::os::unix::fs::symlink(
        canonical.join("parent/content/secret"),
        selected.join("child/content/former-parent"),
    )
    .unwrap();
    write(&selected, "child/content/seed/story.md", b"---\ntype: story\nsource_repo: acme/api\nstory_id: SEED\nsupporting_docs:\n  - ../former-parent/story.md\n---\nSEED_BYTES\n");
    let parentless = remote().files;
    assert_eq!(parentless.len(), 2, "a known selected parentless reader keeps R7 access even when the source registry calls that owner a parent");
    let (parentless_want, parentless_outside) =
        want_for_files(home.path(), &alice(), &target, &parentless);
    assert_eq!((parentless_want.files.len(), parentless_outside), (2, 0));
    let report = hydrate_want(&parentless_want, "parentless-r7");
    assert_eq!(report.status, PrepareStatus::Ready);
    assert_eq!(
        std::fs::read(workspace.join("former-parent/story.md")).unwrap(),
        story("secret").as_bytes()
    );
}
