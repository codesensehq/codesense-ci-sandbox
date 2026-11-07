// SPDX-License-Identifier: MIT
//! SW-XS-8: the explicit publish, locally (on a machine: the wire tests).
use std::collections::BTreeMap;

use super::super::step3::tests::{git, snapshot, write, Fx};
use super::*;
use crate::agents::resolve::tests::{alice, ORG, TEAM};

fn request<'a>(
    fx: &'a Fx,
    target: &'a ManifestTarget,
    paths: &'a [String],
    sets: &'a [String],
    expect: &'a BTreeMap<String, Expect>,
    cancel: Option<&'a dyn Fn() -> bool>,
) -> PublishRequest<'a> {
    PublishRequest {
        home: fx.home.path(),
        principal: &PRINCIPAL,
        destination: Destination::Local { root: &fx.root },
        target,
        paths,
        sets,
        to: None,
        expect,
        include_deletes: false,
        legacy: None,
        operation: "pub-1",
        workspace: "ws".into(),
        agent: "Barb".into(),
        progress: &|_| {},
        validate: &|| Ok(()),
        cancel,
    }
}

static PRINCIPAL: std::sync::LazyLock<Principal> = std::sync::LazyLock::new(alice);

fn publish_all(fx: &Fx) -> PublishReport {
    let target = fx.target();
    let expect = BTreeMap::new();
    publish(&request(fx, &target, &[], &[], &expect, None)).unwrap()
}

fn preview_all(fx: &Fx) -> PublishReport {
    let target = fx.target();
    let expect = BTreeMap::new();
    preview(&request(fx, &target, &[], &[], &expect, None)).unwrap()
}

/// A managed file edited in the workspace goes back to the team file it was
/// copied from; nothing else changes in team content.
#[test]
fn publish_writes_exactly_the_changed_managed_files_back() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[]));
    write(
        &fx.root,
        ".claude/skills/bmad-build/SKILL.md",
        b"# Build, edited\n",
    );
    let before = snapshot(&fx.team);
    let report = publish_all(&fx);
    assert_eq!(report.status, PublishStatus::Published, "{report:?}");
    assert_eq!(report.entries.len(), 1);
    let entry = &report.entries[0];
    assert_eq!(entry.path, ".claude/skills/bmad-build/SKILL.md");
    assert_eq!(entry.action, PublishAction::Update);
    assert_eq!(entry.target_layer, format!("team:{ORG}/{TEAM}"));
    let after = snapshot(&fx.team);
    let changed: Vec<&String> = after
        .keys()
        .filter(|path| before.get(*path) != after.get(*path))
        .collect();
    assert_eq!(changed, vec![".claude/skills/bmad-build/SKILL.md"]);
    assert_eq!(
        after[".claude/skills/bmad-build/SKILL.md"],
        b"# Build, edited\n"
    );
    // Published: the next publish has nothing to do.
    assert_eq!(publish_all(&fx).status, PublishStatus::NothingToPublish);
}

/// §11.11: a team file that changed since it was copied is a conflict and is
/// never written.
#[test]
fn a_team_file_changed_since_the_copy_is_a_conflict_never_written() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&["CLAUDE.md"], &[]));
    write(&fx.root, "CLAUDE.md", b"workspace version");
    write(&fx.team, "CLAUDE.md", b"team version");
    let report = publish_all(&fx);
    assert_eq!(report.status, PublishStatus::Failed);
    assert!(report.entries.is_empty());
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].path, "CLAUDE.md");
    assert_eq!(
        std::fs::read(fx.team.join("CLAUDE.md")).unwrap(),
        b"team version"
    );
}

/// §11.11: a new file under a declared prefix goes to the active team; it is
/// managed from then on, and stays hidden from git (X7).
#[test]
fn a_new_file_under_a_declared_prefix_is_added_to_the_team() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    write(&fx.root, ".claude/skills/new-skill/SKILL.md", b"# New\n");
    let preview = preview_all(&fx);
    assert_eq!(preview.status, PublishStatus::Preview);
    assert_eq!(preview.entries.len(), 1);
    assert_eq!(preview.entries[0].action, PublishAction::Add);
    assert_eq!(
        preview.entries[0].target_path,
        fx.team.join(".claude/skills/new-skill/SKILL.md"),
        "the preview shows the target path"
    );
    let report = publish_all(&fx);
    assert_eq!(report.status, PublishStatus::Published, "{report:?}");
    assert_eq!(
        std::fs::read(fx.team.join(".claude/skills/new-skill/SKILL.md")).unwrap(),
        b"# New\n"
    );
    let entry = &fx.record().entries[".claude/skills/new-skill/SKILL.md"];
    assert_eq!(entry.origin, "step3:pub-1");
    assert_eq!(
        git(
            &fx.root,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        ""
    );
}

/// A new file whose target already holds other bytes has no base: conflict.
#[test]
fn a_new_file_over_a_different_team_file_is_a_conflict() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    write(&fx.team, ".claude/skills/other/SKILL.md", b"team's");
    write(&fx.root, ".claude/skills/other/SKILL.md", b"mine");
    let report = publish_all(&fx);
    assert_eq!(report.conflicts.len(), 1, "{report:?}");
    assert_eq!(
        std::fs::read(fx.team.join(".claude/skills/other/SKILL.md")).unwrap(),
        b"team's"
    );
}

/// §11.11 + review r1: with `includeDeletes`, publish removes only managed
/// paths absent from the worktree.
#[test]
fn a_managed_file_removed_in_the_workspace_is_removed_from_the_team() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/commands"]));
    std::fs::remove_file(fx.root.join(".claude/commands/bmad-help.md")).unwrap();
    let target = fx.target();
    let expect = BTreeMap::new();
    let mut deleting = request(&fx, &target, &[], &[], &expect, None);
    deleting.include_deletes = true;
    let report = publish(&deleting).unwrap();
    assert_eq!(report.entries.len(), 1, "{report:?}");
    assert_eq!(report.entries[0].action, PublishAction::Remove);
    assert!(!fx.team.join(".claude/commands/bmad-help.md").exists());
    assert!(!fx
        .record()
        .entries
        .contains_key(".claude/commands/bmad-help.md"));
}

/// AC2 (locally): the preview lists what a publish would write, and writes
/// nothing, neither in team content nor in the record.
#[test]
fn a_preview_lists_the_changes_and_writes_nothing() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[]));
    write(&fx.root, "CLAUDE.md", b"edited");
    write(&fx.root, ".claude/commands/bmad-help.md", b"edited too");
    let team = snapshot(&fx.team);
    let record = fx.record();
    let report = preview_all(&fx);
    let paths: Vec<&str> = report.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, vec![".claude/commands/bmad-help.md", "CLAUDE.md"]);
    assert_eq!(snapshot(&fx.team), team);
    assert_eq!(fx.record(), record);
}

/// The selected form's expected hashes: a workspace that changed since the
/// caller looked is a conflict.
#[test]
fn an_expected_workspace_hash_that_no_longer_holds_is_a_conflict() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&["CLAUDE.md"], &[]));
    write(&fx.root, "CLAUDE.md", b"edited");
    let target = fx.target();
    let mut expect = BTreeMap::new();
    expect.insert(
        "CLAUDE.md".to_string(),
        Expect {
            source: None,
            workspace: Some([9; 32]),
            target: None,
            stat: None,
        },
    );
    let paths = vec!["CLAUDE.md".to_string()];
    let report = publish(&request(&fx, &target, &paths, &[], &expect, None)).unwrap();
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(
        std::fs::read(fx.team.join("CLAUDE.md")).unwrap(),
        b"# Team\n"
    );
}

/// A cancelled publish writes nothing to team content.
#[test]
fn a_cancelled_publish_writes_nothing() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[]));
    write(&fx.root, "CLAUDE.md", b"edited");
    let team = snapshot(&fx.team);
    let target = fx.target();
    let expect = BTreeMap::new();
    let cancel = || true;
    let report = publish(&request(&fx, &target, &[], &[], &expect, Some(&cancel))).unwrap();
    assert_eq!(report.status, PublishStatus::Cancelled);
    assert_eq!(snapshot(&fx.team), team);
}

/// The team write is one transaction: a race on a later entry rolls back
/// the earlier ones.
#[test]
fn a_race_on_one_entry_rolls_back_the_whole_write() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.md");
    let b = dir.path().join("b.md");
    std::fs::write(&a, b"a0").unwrap();
    std::fs::write(&b, b"b-moved-on").unwrap();
    let planned = |path: &str, target: &std::path::Path, team: &[u8], new: &[u8]| Planned {
        entry: PublishEntry {
            path: path.into(),
            action: PublishAction::Update,
            target_layer: "team:o/t".into(),
            target_path: target.to_path_buf(),
            bytes: new.len() as u64,
        },
        team_expected: Some(*blake3::hash(team).as_bytes()),
        workspace_hash: Some(*blake3::hash(new).as_bytes()),
        mode: 0o644,
        stat: None,
        managed: None,
    };
    let first = planned("a.md", &a, b"a0", b"a1");
    let second = planned("b.md", &b, b"b0", b"b1");
    let mut bytes = BTreeMap::new();
    bytes.insert("a.md".to_string(), b"a1".to_vec());
    bytes.insert("b.md".to_string(), b"b1".to_vec());
    let roots = vec![(
        "team:o/t".to_string(),
        crate::context_repo::inheritance::ContentRoot::own(dir.path().to_path_buf()),
    )];
    let result = write_transaction(&[&first, &second], &bytes, &roots);
    assert_eq!(result.unwrap_err().path, "b.md");
    assert_eq!(std::fs::read(&a).unwrap(), b"a0", "rolled back");
    assert_eq!(std::fs::read(&b).unwrap(), b"b-moved-on");
}

/// Review r1: a rollback restores a target only while it still holds what
/// this publish wrote; a writer that changed it since keeps its bytes.
#[test]
fn a_rollback_never_clobbers_a_later_writer() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.md");
    std::fs::write(&a, b"a0").unwrap();
    let missing = dir.path().join("sub/missing.md");
    let planned = |path: &str, target: &std::path::Path, team: Option<&[u8]>, new: &[u8]| Planned {
        entry: PublishEntry {
            path: path.into(),
            action: PublishAction::Update,
            target_layer: "team:o/t".into(),
            target_path: target.to_path_buf(),
            bytes: new.len() as u64,
        },
        team_expected: team.map(|t| *blake3::hash(t).as_bytes()),
        workspace_hash: Some(*blake3::hash(new).as_bytes()),
        mode: 0o644,
        stat: None,
        managed: None,
    };
    let first = planned("a.md", &a, Some(b"a0"), b"a1");
    // The second entry fails (its expected base is not there).
    let second = planned("sub/missing.md", &missing, Some(b"x"), b"y");
    let mut bytes = BTreeMap::new();
    bytes.insert("a.md".to_string(), b"a1".to_vec());
    bytes.insert("sub/missing.md".to_string(), b"y".to_vec());
    let roots = vec![(
        "team:o/t".to_string(),
        crate::context_repo::inheritance::ContentRoot::own(dir.path().to_path_buf()),
    )];
    // Another writer replaces a.md between our write and the rollback: the
    // failing entry's read error triggers the rollback; we simulate the
    // writer by writing a.md from a watcher-free hook — the simplest proof is
    // the rollback primitive itself: restore only over our own bytes.
    let result = write_transaction(&[&first, &second], &bytes, &roots);
    assert!(result.is_err());
    assert_eq!(std::fs::read(&a).unwrap(), b"a0", "restored over our bytes");
    std::fs::write(&a, b"someone else").unwrap();
    assert!(
        !crate::content_engine::atomic_io::write_tmp_fsync_rename_expected(
            &a,
            b"a0",
            Some(*blake3::hash(b"a1").as_bytes())
        )
        .unwrap()
    );
    assert_eq!(std::fs::read(&a).unwrap(), b"someone else");
}

/// Review r1 M1: a target outside every content root is never written.
#[test]
fn a_target_outside_team_content_is_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let team = dir.path().join("team");
    std::fs::create_dir_all(&team).unwrap();
    let escape = team.join("../outside.md");
    let planned = Planned {
        entry: PublishEntry {
            path: "x.md".into(),
            action: PublishAction::Add,
            target_layer: "team:o/t".into(),
            target_path: escape.clone(),
            bytes: 1,
        },
        team_expected: None,
        workspace_hash: Some(*blake3::hash(b"x").as_bytes()),
        mode: 0o644,
        stat: None,
        managed: None,
    };
    let mut bytes = BTreeMap::new();
    bytes.insert("x.md".to_string(), b"x".to_vec());
    let roots = vec![(
        "team:o/t".to_string(),
        crate::context_repo::inheritance::ContentRoot::own(team),
    )];
    assert!(write_transaction(&[&planned], &bytes, &roots).is_err());
    assert!(!dir.path().join("outside.md").exists());
}

/// Review r1 M1: a machine that names a path escaping the workspace
/// (`docs/../../.ssh/…`) is refused: nothing is planned, read or written.
#[test]
fn a_hostile_manifest_path_is_refused() {
    let fx = Fx::new();
    let machine = crate::agents::prepare::tests::Machine::new();
    machine.put("docs/ok.md", b"fine");
    machine.put(
        "docs/../../.ssh/authorized_keys",
        b"ssh-ed25519 AAAA attacker",
    );
    let target = fx.target();
    let expect = BTreeMap::new();
    let sets = vec!["docs".to_string()];
    let mut hostile = request(&fx, &target, &[], &sets, &expect, None);
    hostile.destination = Destination::Remote { io: &machine };
    let before = snapshot(fx.home.path());
    let error = preview(&hostile).unwrap_err();
    assert!(error.contains("unsafe path"), "{error}");
    let error = publish(&hostile).unwrap_err();
    assert!(error.contains("unsafe path"), "{error}");
    assert_eq!(snapshot(fx.home.path()), before, "nothing written anywhere");
}

/// Review r1 decision: a default publish never deletes team content. After
/// `git clean -fdx` wipes every managed copy, the team is untouched and the
/// would-be removals are listed (preview and publish alike).
#[test]
fn a_git_clean_never_wipes_team_content() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[]));
    let managed = fx.record().entries.len();
    assert!(managed > 1);
    git(&fx.root, &["clean", "-fdxq"]);
    let team = snapshot(&fx.team);
    let preview = preview_all(&fx);
    assert!(preview.entries.is_empty(), "{preview:?}");
    assert_eq!(preview.withheld_removals.len(), managed);
    let report = publish_all(&fx);
    assert_eq!(report.status, PublishStatus::NothingToPublish, "{report:?}");
    assert_eq!(report.withheld_removals.len(), managed);
    assert_eq!(snapshot(&fx.team), team, "team content untouched");
    assert_eq!(fx.record().entries.len(), managed, "still managed");
}

/// Review r1: some entries written and some conflicted is its own status.
#[test]
fn a_partial_publish_reports_published_with_conflicts() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[]));
    write(&fx.root, "CLAUDE.md", b"workspace version");
    write(&fx.team, "CLAUDE.md", b"team moved on");
    write(&fx.root, ".claude/commands/bmad-help.md", b"clean edit");
    let report = publish_all(&fx);
    assert_eq!(
        report.status,
        PublishStatus::PublishedWithConflicts,
        "{report:?}"
    );
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.conflicts.len(), 1);
}

/// Review r1: files over 4 MiB are skipped in the plan with their reason,
/// never failed half-way.
#[test]
fn a_file_over_four_mib_is_skipped_with_a_reason() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    let big = vec![b'x'; (MAX_PUBLISH_BYTES + 1) as usize];
    write(&fx.root, ".claude/skills/big/blob.txt", &big);
    let report = publish_all(&fx);
    assert!(report.entries.is_empty(), "{report:?}");
    assert!(
        report
            .skipped
            .iter()
            .any(|(path, why)| path == ".claude/skills/big/blob.txt" && why.contains("MiB")),
        "{report:?}"
    );
    assert!(!fx.team.join(".claude/skills/big/blob.txt").exists());
}

/// Review r1 perf: a new-file walk never descends into dependency trees.
#[test]
fn a_new_file_walk_skips_dependency_trees() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    write(&fx.root, ".claude/skills/node_modules/pkg/index.js", b"dep");
    write(&fx.root, ".claude/skills/new/SKILL.md", b"# New\n");
    let report = preview_all(&fx);
    let paths: Vec<&str> = report.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, vec![".claude/skills/new/SKILL.md"]);
}

/// SW-XS-8b: `route_file_to_context` runs on the engine — a one-path publish
/// to the team tier. The file lands in team content, is managed and hidden
/// from git; routing again is a no-op; unrouting removes the team copy (it
/// still holds the workspace bytes) and shows the file in git again.
#[test]
fn route_publishes_one_file_to_the_team_and_unroute_reverses_it() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[]));
    write(&fx.root, "notes/plan.md", b"# Plan\n");
    let target = fx.target();
    let routed = route(
        fx.home.path(),
        &PRINCIPAL,
        &fx.root,
        &target,
        "notes/plan.md",
        "r-1",
        None,
    )
    .unwrap();
    assert_eq!(routed, RouteResult::Routed);
    assert_eq!(
        std::fs::read(fx.team.join("notes/plan.md")).unwrap(),
        b"# Plan\n"
    );
    assert!(fx.record().entries.contains_key("notes/plan.md"));
    assert_eq!(
        git(
            &fx.root,
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--",
                "notes"
            ]
        ),
        ""
    );
    let again = route(
        fx.home.path(),
        &PRINCIPAL,
        &fx.root,
        &target,
        "notes/plan.md",
        "r-2",
        None,
    )
    .unwrap();
    assert_eq!(again, RouteResult::AlreadyRouted);

    unroute(
        fx.home.path(),
        &PRINCIPAL,
        &fx.root,
        &target,
        "notes/plan.md",
    )
    .unwrap();
    assert!(!fx.team.join("notes/plan.md").exists());
    assert!(!fx.record().entries.contains_key("notes/plan.md"));
    assert_eq!(
        git(
            &fx.root,
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--",
                "notes"
            ]
        ),
        "?? notes/plan.md\n"
    );
    // Idempotent.
    unroute(
        fx.home.path(),
        &PRINCIPAL,
        &fx.root,
        &target,
        "notes/plan.md",
    )
    .unwrap();
}

/// Route refusals write nothing: a tracked file is code, a different team
/// file at the path is a collision, a missing file is refused.
#[test]
fn route_refusals_write_nothing() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[]));
    let target = fx.target();
    write(&fx.root, "src/main.rs", b"fn main() {}\n");
    git(&fx.root, &["add", "src/main.rs"]);
    write(&fx.root, "notes/a.md", b"workspace");
    write(&fx.team, "notes/a.md", b"team");
    let before = snapshot(&fx.team);
    let r = |p: &str| route(fx.home.path(), &PRINCIPAL, &fx.root, &target, p, "r", None).unwrap();
    assert_eq!(r("src/main.rs"), RouteResult::AlreadyTracked);
    assert!(matches!(r("notes/a.md"), RouteResult::Collision { .. }));
    assert_eq!(r("notes/missing.md"), RouteResult::Refused);
    assert_eq!(r("../escape.md"), RouteResult::NotProjectable);
    assert_eq!(snapshot(&fx.team), before);
}

/// Review r1: a publish settles a batch whose reply was lost (recovered from
/// the machine) into the record before it plans, so what that batch applied
/// is managed — never dropped.
#[test]
fn a_publish_settles_a_cut_batch_first() {
    let fx = Fx::new();
    let machine = crate::agents::prepare::tests::Machine::new();
    *machine.lose_reply.lock().unwrap() = true;
    let legacy = || Ok(Vec::new());
    let want = fx.want(&["CLAUDE.md"], &[]);
    let cut = super::super::step3::hydrate(super::super::step3::Hydrate {
        home: fx.home.path(),
        principal: &PRINCIPAL,
        destination: Destination::Remote { io: &machine },
        want: &want,
        operation: "op-cut",
        workspace: "ws".into(),
        agent: "Barb".into(),
        prune: false,
        overwrite_modified: false,
        progress: &|_| {},
        validate: &|| Ok(()),
        cancel: None,
        legacy: &legacy,
    });
    assert!(cut.is_err(), "the reply was lost");
    let before = ManagedRecord::load(fx.home.path(), &PRINCIPAL, &machine.identity);
    assert!(!before.pending.is_empty());
    assert!(!before.entries.contains_key("CLAUDE.md"));
    let target = fx.target();
    let expect = BTreeMap::new();
    let mut remote = request(&fx, &target, &[], &[], &expect, None);
    remote.destination = Destination::Remote { io: &machine };
    let report = publish(&remote).unwrap();
    assert_eq!(report.status, PublishStatus::NothingToPublish, "{report:?}");
    let after = ManagedRecord::load(fx.home.path(), &PRINCIPAL, &machine.identity);
    assert!(after.pending.is_empty(), "settled");
    assert!(
        after.entries.contains_key("CLAUDE.md"),
        "what the batch applied is managed"
    );
}

/// Review r1 M1: unrouting a file a hydrate copied in only unmanages it; the
/// team file it came from is never deleted.
#[test]
fn unrouting_a_hydrated_file_never_deletes_team_content() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&["CLAUDE.md"], &[]));
    let target = fx.target();
    let team_copy = std::fs::read(fx.team.join("CLAUDE.md")).unwrap();
    unroute(fx.home.path(), &PRINCIPAL, &fx.root, &target, "./CLAUDE.md").unwrap();
    assert_eq!(std::fs::read(fx.team.join("CLAUDE.md")).unwrap(), team_copy);
    assert!(!fx.record().entries.contains_key("CLAUDE.md"));
    assert_eq!(
        git(
            &fx.root,
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--",
                "CLAUDE.md"
            ]
        ),
        "?? CLAUDE.md\n"
    );
}

/// Review r1 M3: routing a file already managed here is AlreadyRouted and
/// publishes nothing (an edit to a hydrated copy is not routed over the team).
#[test]
fn routing_a_managed_file_is_already_routed_and_publishes_nothing() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&["CLAUDE.md"], &[]));
    write(&fx.root, "CLAUDE.md", b"an edit");
    let target = fx.target();
    let before = snapshot(&fx.team);
    let result = route(
        fx.home.path(),
        &PRINCIPAL,
        &fx.root,
        &target,
        "CLAUDE.md",
        "r",
        None,
    )
    .unwrap();
    assert_eq!(result, RouteResult::AlreadyRouted);
    assert_eq!(snapshot(&fx.team), before);
}

/// Review r1 M4: routing a file whose bytes team content already holds adopts
/// it as managed AND hides it from git (no write needed).
#[test]
fn routing_bytes_the_team_already_has_still_hides_the_file() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[]));
    write(&fx.team, "notes/shared.md", b"same bytes");
    write(&fx.root, "notes/shared.md", b"same bytes");
    let target = fx.target();
    let result = route(
        fx.home.path(),
        &PRINCIPAL,
        &fx.root,
        &target,
        "notes/shared.md",
        "r",
        None,
    )
    .unwrap();
    assert_eq!(result, RouteResult::AlreadyRouted);
    assert!(fx.record().entries.contains_key("notes/shared.md"));
    assert!(fx.record().entries["notes/shared.md"]
        .origin
        .starts_with("step3:adopt:"));
    assert_eq!(
        git(
            &fx.root,
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--",
                "notes"
            ]
        ),
        ""
    );
    // Review r2 M1: the team file existed before the route; unrouting the
    // adopted file only unmanages it — the team file survives.
    unroute(
        fx.home.path(),
        &PRINCIPAL,
        &fx.root,
        &target,
        "notes/shared.md",
    )
    .unwrap();
    assert_eq!(
        std::fs::read(fx.team.join("notes/shared.md")).unwrap(),
        b"same bytes"
    );
    assert!(!fx.record().entries.contains_key("notes/shared.md"));
}

/// Review r2: a pre-upgrade worktree's old projected file (known only to the
/// legacy lockfile) is AlreadyRouted: the route seeds the record first.
#[test]
fn routing_an_old_projected_file_is_already_routed() {
    let fx = Fx::new();
    let team_bytes = std::fs::read(fx.team.join("CLAUDE.md")).unwrap();
    write(&fx.root, "CLAUDE.md", &team_bytes);
    let rows = vec![(
        "CLAUDE.md".to_string(),
        *blake3::hash(&team_bytes).as_bytes(),
        format!("team:{ORG}/{TEAM}"),
    )];
    let legacy = move || Ok(rows.clone());
    let target = fx.target();
    let before = snapshot(&fx.team);
    let result = route(
        fx.home.path(),
        &PRINCIPAL,
        &fx.root,
        &target,
        "CLAUDE.md",
        "r",
        Some(&legacy),
    )
    .unwrap();
    assert_eq!(result, RouteResult::AlreadyRouted);
    assert_eq!(snapshot(&fx.team), before);
    assert!(fx.record().seeded);
}

/// Review r1 M2: a pre-upgrade worktree whose record was never seeded (only
/// the legacy lockfile knows its projected files) still publishes its edits
/// back: preview and publish seed from the lockfile rows.
#[test]
fn an_unseeded_lockfile_only_worktree_publishes_its_edits() {
    let fx = Fx::new();
    // What the old projection wrote: CLAUDE.md, recorded only in the lockfile.
    let team_bytes = std::fs::read(fx.team.join("CLAUDE.md")).unwrap();
    write(&fx.root, "CLAUDE.md", &team_bytes);
    git(&fx.root, &["status"]);
    write(&fx.root, "CLAUDE.md", b"edited after the upgrade");
    let rows = vec![(
        "CLAUDE.md".to_string(),
        *blake3::hash(&team_bytes).as_bytes(),
        format!("team:{ORG}/{TEAM}"),
    )];
    let legacy = move || Ok(rows.clone());
    let target = fx.target();
    let expect = BTreeMap::new();
    let mut seeded = request(&fx, &target, &[], &[], &expect, None);
    seeded.legacy = Some(&legacy);
    let preview = preview(&seeded).unwrap();
    assert_eq!(
        preview
            .entries
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>(),
        vec!["CLAUDE.md"]
    );
    assert!(!fx.record().seeded, "a preview writes nothing");
    let report = publish(&seeded).unwrap();
    assert_eq!(report.status, PublishStatus::Published, "{report:?}");
    assert_eq!(
        std::fs::read(fx.team.join("CLAUDE.md")).unwrap(),
        b"edited after the upgrade"
    );
    assert!(fx.record().seeded);
}

/// Review r1: a launch's merged file (generated) is never published back.
#[test]
fn a_generated_file_is_never_published() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&["CLAUDE.md"], &[]));
    let identity = crate::content_engine::context_lifecycle::local_identity(&fx.root);
    let mut record = fx.record();
    record.entries.get_mut("CLAUDE.md").unwrap().source_layer =
        crate::agents::prepare::want::GENERATED_LAYER.into();
    record.save(fx.home.path(), &PRINCIPAL, &identity).unwrap();
    write(&fx.root, "CLAUDE.md", b"merged output");
    let before = snapshot(&fx.team);
    let report = publish_all(&fx);
    assert!(report.entries.is_empty(), "{report:?}");
    assert!(report.skipped.iter().any(|(p, _)| p == "CLAUDE.md"));
    assert_eq!(snapshot(&fx.team), before);
}

/// SW-XS-27: discarding a new file deletes exactly it, only while it still
/// holds what the plan saw — and never a copied file, a file outside the
/// team folders, or team content.
#[test]
fn discard_new_files_deletes_only_unchanged_new_files() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    write(&fx.root, ".claude/skills/new/SKILL.md", b"# New\n");
    write(&fx.root, "loose.md", b"not in a team folder");
    let target = fx.target();
    let expect = BTreeMap::new();
    let team_before = snapshot(&fx.team);
    let paths = vec![".claude/skills/new/SKILL.md".to_string()];
    let ok = discard_new_files(&request(&fx, &target, &paths, &[], &expect, None)).unwrap();
    assert_eq!(ok.removed, paths);
    assert!(!fx.root.join(".claude/skills/new/SKILL.md").exists());
    assert_eq!(snapshot(&fx.team), team_before);
    for refused in [
        vec!["loose.md".to_string()],
        vec![".claude/skills/bmad-build/SKILL.md".to_string()],
        vec![".claude/skills".to_string()],
    ] {
        assert!(
            discard_new_files(&request(&fx, &target, &refused, &[], &expect, None)).is_err(),
            "{refused:?}"
        );
    }
    assert!(fx.root.join("loose.md").exists());
}
