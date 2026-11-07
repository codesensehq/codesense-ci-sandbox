// SPDX-License-Identifier: MIT
//! SW-XS-27: the read-only count, locally (on a machine: the wire tests).
use super::super::step3::tests::{git, snapshot, write, Fx};
use super::*;
use crate::agents::resolve::tests::{alice, ORG, TEAM};

static PRINCIPAL: std::sync::LazyLock<Principal> = std::sync::LazyLock::new(alice);

fn count_with(fx: &Fx, legacy: Option<&dyn Fn() -> Result<LegacyRows, String>>) -> WorkspaceEdits {
    let target = fx.target();
    count(&CountRequest {
        home: fx.home.path(),
        principal: &PRINCIPAL,
        destination: Destination::Local { root: &fx.root },
        target: &target,
        legacy,
    })
}

fn count_of(fx: &Fx) -> WorkspaceEdits {
    count_with(fx, None)
}

fn listed(edits: &WorkspaceEdits) -> Vec<(&str, EditKind)> {
    edits
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.kind))
        .collect()
}

fn team() -> EditDestination {
    EditDestination::Team {
        name: TEAM.to_string(),
    }
}

/// The record file's bytes, and every file beside it (what counting must
/// never change).
fn record_dir_state(fx: &Fx) -> std::collections::BTreeMap<String, Vec<u8>> {
    let identity = crate::content_engine::context_lifecycle::local_identity(&fx.root);
    let path = super::super::record::record_path(fx.home.path(), &alice(), &identity);
    match path.parent() {
        Some(dir) if dir.exists() => snapshot(dir),
        _ => Default::default(),
    }
}

/// AC1: an edited copy is listed as changed, with where it publishes to;
/// put back to the copied bytes it is gone.
#[test]
fn an_edited_copy_is_listed_until_it_matches_the_copy_again() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    let original = fx.read(".claude/skills/bmad-build/SKILL.md").unwrap();
    assert_eq!(count_of(&fx).counts.total(), 0, "nothing edited yet");
    write(
        &fx.root,
        ".claude/skills/bmad-build/SKILL.md",
        b"# Build, edited\n",
    );
    let edits = count_of(&fx);
    assert_eq!(edits.state, EditsState::Checked);
    assert_eq!(
        listed(&edits),
        vec![(".claude/skills/bmad-build/SKILL.md", EditKind::Changed)]
    );
    assert_eq!(edits.counts.changed, 1);
    assert_eq!(edits.files[0].destination, team());
    assert!(edits.files[0].changed_at_ms.is_some());
    assert_eq!(edits.last_changed_at_ms, edits.files[0].changed_at_ms);
    write(&fx.root, ".claude/skills/bmad-build/SKILL.md", &original);
    assert_eq!(count_of(&fx).counts.total(), 0, "back to the copy");
}

/// AC2: a new file under a team folder is listed as new.
#[test]
fn a_new_file_under_a_team_folder_is_listed_as_new() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    write(&fx.root, ".claude/skills/new-skill/SKILL.md", b"# New\n");
    write(&fx.root, "src/main.rs", b"fn main() {}\n");
    let edits = count_of(&fx);
    assert_eq!(
        listed(&edits),
        vec![(".claude/skills/new-skill/SKILL.md", EditKind::New)],
        "only files under the workspace's team folders"
    );
    assert_eq!(edits.files[0].destination, team());
}

/// AC3: CodeSense's own local and generated files and repo-tracked files are
/// never listed — even when an old lockfile recorded them as copies (what
/// the SW-XS-4 notice counted).
#[test]
fn local_generated_and_tracked_files_are_never_listed() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&["CLAUDE.md"], &[".claude/skills"]));
    // Local-only files, recorded as copies by the old projection.
    let base = *blake3::hash(b"as copied").as_bytes();
    let rows: LegacyRows = [
        ".claude/settings.local.json",
        ".claude/settings.json",
        ".mcp.json",
        ".codex/config.toml",
        ".claude/scheduled_tasks.lock",
    ]
    .iter()
    .map(|p| (p.to_string(), base, format!("team:{ORG}/{TEAM}")))
    .collect();
    for (path, _, _) in &rows {
        write(&fx.root, path, b"edited locally");
    }
    // Junk dropped under a team folder.
    write(&fx.root, ".claude/skills/x/wvbuild-lock", b"lock");
    // A generated launch file.
    let identity = crate::content_engine::context_lifecycle::local_identity(&fx.root);
    let mut record = fx.record();
    record.entries.get_mut("CLAUDE.md").unwrap().source_layer =
        super::super::want::GENERATED_LAYER.into();
    record.save(fx.home.path(), &alice(), &identity).unwrap();
    write(&fx.root, "CLAUDE.md", b"merged output");
    // A file the repo tracks, under a team folder.
    write(&fx.root, ".claude/skills/tracked/SKILL.md", b"# Tracked\n");
    git(&fx.root, &["add", "-f", ".claude/skills/tracked/SKILL.md"]);
    git(&fx.root, &["commit", "-qm", "track"]);
    // An unseeded record would take the rows; this one is seeded already,
    // so seed a fresh record the way an old workspace has none.
    record.seeded = false;
    record.save(fx.home.path(), &alice(), &identity).unwrap();
    let legacy = move || Ok(rows.clone());
    let edits = count_with(&fx, Some(&legacy));
    assert_eq!(edits.state, EditsState::Checked, "{edits:?}");
    assert!(edits.files.is_empty(), "{:?}", listed(&edits));
}

/// AC4: a copy edited here whose team file also changed is a conflict.
#[test]
fn edited_here_and_in_the_team_is_a_conflict() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&["CLAUDE.md"], &[]));
    write(&fx.root, "CLAUDE.md", b"workspace version");
    write(&fx.team, "CLAUDE.md", b"team version");
    let edits = count_of(&fx);
    assert_eq!(listed(&edits), vec![("CLAUDE.md", EditKind::Conflict)]);
    assert_eq!(edits.counts.conflicts, 1);
    assert!(edits.files[0].reason.is_some());
}

/// A copy removed here (the team still holds it) is listed as removed.
#[test]
fn a_copy_removed_here_is_listed_as_removed() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&["CLAUDE.md"], &[]));
    std::fs::remove_file(fx.root.join("CLAUDE.md")).unwrap();
    let edits = count_of(&fx);
    assert_eq!(listed(&edits), vec![("CLAUDE.md", EditKind::Removed)]);
    assert_eq!(edits.files[0].destination, team());
}

/// AC5: counting writes nothing — not team content, not the workspace, not
/// the record — and a corrupt record is reported, never moved aside or read
/// as "no edits".
#[test]
fn counting_writes_nothing_and_a_corrupt_record_is_unknown_and_untouched() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&["CLAUDE.md"], &[".claude/skills"]));
    write(&fx.root, "CLAUDE.md", b"edited");
    write(&fx.root, ".claude/skills/new/SKILL.md", b"# New\n");
    let team_before = snapshot(&fx.team);
    let workspace_before = snapshot(&fx.root);
    let records_before = record_dir_state(&fx);
    assert_eq!(count_of(&fx).counts.total(), 2);
    assert_eq!(snapshot(&fx.team), team_before);
    assert_eq!(snapshot(&fx.root), workspace_before);
    assert_eq!(record_dir_state(&fx), records_before);

    let identity = crate::content_engine::context_lifecycle::local_identity(&fx.root);
    let path = super::super::record::record_path(fx.home.path(), &alice(), &identity);
    std::fs::write(&path, b"{ not json").unwrap();
    let records_before = record_dir_state(&fx);
    let edits = count_of(&fx);
    assert_eq!(edits.state, EditsState::Unknown);
    assert!(edits.reason.is_some());
    assert_eq!(edits.counts.total(), 0);
    assert_eq!(
        record_dir_state(&fx),
        records_before,
        "the corrupt record is left exactly where it was"
    );
}

/// A workspace prepared before the managed record existed is counted from
/// its old lockfile rows, in memory: no record file appears.
#[test]
fn an_old_lockfile_only_workspace_is_counted_without_writing_a_record() {
    let fx = Fx::new();
    let team_bytes = std::fs::read(fx.team.join("CLAUDE.md")).unwrap();
    write(&fx.root, "CLAUDE.md", b"edited after the upgrade");
    let rows: LegacyRows = vec![(
        "CLAUDE.md".to_string(),
        *blake3::hash(&team_bytes).as_bytes(),
        format!("team:{ORG}/{TEAM}"),
    )];
    let legacy = move || Ok(rows.clone());
    let edits = count_with(&fx, Some(&legacy));
    assert_eq!(listed(&edits), vec![("CLAUDE.md", EditKind::Changed)]);
    assert!(record_dir_state(&fx).is_empty(), "no record was written");

    let broken = || Err("busy".to_string());
    let edits = count_with(&fx, Some(&broken));
    assert_eq!(edits.state, EditsState::Unknown, "never read as no edits");
}

/// §9 budget: nothing changed since the copy → no workspace file is read.
#[test]
fn an_unchanged_workspace_is_counted_without_hashing() {
    let fx = Fx::new();
    for i in 0..200 {
        write(
            &fx.team,
            &format!("docs/d{}/f{i}.md", i % 7),
            format!("doc {i}").as_bytes(),
        );
    }
    let want = fx.want(&[], &["docs"]);
    fx.hydrate(&want);
    // The record's save must be a tick past the copies' mtimes (racy-clean).
    std::thread::sleep(std::time::Duration::from_millis(30));
    fx.hydrate(&want);
    let edits = count_of(&fx);
    assert_eq!(edits.counts.total(), 0, "{:?}", listed(&edits));
    assert_eq!(edits.hashed, 0, "stat-trusted: nothing read");
}

#[test]
fn destinations_are_labelled_by_tier() {
    let target = ManifestTarget::team("acme/api", "backend");
    assert_eq!(
        destination_of("team:acme/api/backend", &target),
        EditDestination::Team {
            name: "backend".into()
        }
    );
    assert_eq!(
        destination_of("team:acme/api/platform", &target),
        EditDestination::ParentTeam {
            name: "platform".into()
        }
    );
    assert_eq!(
        destination_of("personal:acme/api", &target),
        EditDestination::Private
    );
    assert_eq!(
        destination_of("personal", &target),
        EditDestination::Private
    );
    assert_eq!(
        destination_of("repo:github.com/acme/api", &target),
        EditDestination::Repo
    );
}

/// An edit a publish can't move (too large) is listed, never invisible.
#[test]
fn an_edit_too_large_to_publish_is_listed_with_its_reason() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    let big = vec![b'x'; 4 * 1024 * 1024 + 1];
    write(&fx.root, ".claude/skills/big/blob.txt", &big);
    let edits = count_of(&fx);
    assert_eq!(
        listed(&edits),
        vec![(".claude/skills/big/blob.txt", EditKind::CannotPublish)]
    );
    assert_eq!(edits.counts.cannot_publish, 1);
    assert!(edits.files[0].reason.as_deref().unwrap().contains("MiB"));
}

#[test]
fn unknown_and_other_org_tiers_are_labelled_as_they_are() {
    let target = ManifestTarget::team("acme/api", "backend");
    assert_eq!(
        destination_of("team:other/org/platform", &target),
        EditDestination::ParentTeam {
            name: "other/org/platform".into()
        }
    );
    assert_eq!(
        destination_of("legacy:TeamProjection", &target),
        EditDestination::Other {
            label: "legacy:TeamProjection".into()
        }
    );
}

/// An edit to a copy whose TEAM file is too large to publish is still
/// listed (a publish can't move it either way).
#[test]
fn an_edit_over_a_team_file_too_large_to_publish_is_listed() {
    let fx = Fx::new();
    let big = vec![b'x'; 4 * 1024 * 1024 + 1];
    write(&fx.team, "assets/diagram.bin", &big);
    fx.hydrate(&fx.want(&["assets/diagram.bin"], &[]));
    assert_eq!(count_of(&fx).counts.total(), 0, "untouched: nothing");
    write(&fx.root, "assets/diagram.bin", b"edited small");
    let edits = count_of(&fx);
    assert_eq!(
        listed(&edits),
        vec![("assets/diagram.bin", EditKind::CannotPublish)]
    );
}

/// Review: a count reads a new file only when its bytes decide the answer —
/// a team file at its path. A new file with nothing there is listed unread.
#[test]
fn a_new_file_is_read_only_when_the_team_has_a_file_at_its_path() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    std::thread::sleep(std::time::Duration::from_millis(30));
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    write(&fx.root, ".claude/skills/new/SKILL.md", b"# New\n");
    let edits = count_of(&fx);
    assert_eq!(
        listed(&edits),
        vec![(".claude/skills/new/SKILL.md", EditKind::New)]
    );
    assert_eq!(edits.hashed, 0, "nothing to compare it with: not read");
    assert!(edits.files[0].workspace_hash.is_none());
    // The team now has a file there: the new file is read to compare.
    write(&fx.team, ".claude/skills/new/SKILL.md", b"# New\n");
    let edits = count_of(&fx);
    assert!(
        edits.files.is_empty(),
        "already the team's: {:?}",
        listed(&edits)
    );
    // A changed copy carries the hashes an action pins.
    write(
        &fx.root,
        ".claude/skills/bmad-build/SKILL.md",
        b"# Build, edited\n",
    );
    let edits = count_of(&fx);
    let changed = edits
        .files
        .iter()
        .find(|f| f.kind == EditKind::Changed)
        .unwrap();
    assert_eq!(
        changed.workspace_hash.as_deref(),
        Some(format!("blake3:{}", blake3::hash(b"# Build, edited\n").to_hex()).as_str())
    );
    assert!(changed.team_hash.is_some());
}

/// Review: an unread new file is pinned by its stat; a rewrite after the
/// count makes a publish of it a conflict, never a write.
#[test]
fn an_unread_new_file_is_pinned_by_its_stat() {
    let fx = Fx::new();
    fx.hydrate(&fx.want(&[], &[".claude/skills"]));
    write(&fx.root, ".claude/skills/new/SKILL.md", b"# New\n");
    let edits = count_of(&fx);
    let stat = edits.files[0]
        .workspace_stat
        .clone()
        .expect("listed with its stat");
    let (size, mtime) = stat.split_once(':').unwrap();
    let pinned = (size.parse::<u64>().unwrap(), mtime.parse::<i64>().unwrap());
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(&fx.root, ".claude/skills/new/SKILL.md", b"# Rewritten\n");
    let target = fx.target();
    let mut expect = std::collections::BTreeMap::new();
    expect.insert(
        ".claude/skills/new/SKILL.md".to_string(),
        super::super::publish::Expect {
            stat: Some(pinned),
            ..Default::default()
        },
    );
    let paths = vec![".claude/skills/new/SKILL.md".to_string()];
    let report = super::super::publish::publish(&super::super::publish::PublishRequest {
        home: fx.home.path(),
        principal: &PRINCIPAL,
        destination: Destination::Local { root: &fx.root },
        target: &target,
        paths: &paths,
        sets: &[],
        to: None,
        expect: &expect,
        include_deletes: false,
        legacy: None,
        operation: "pub-stat",
        workspace: "ws".into(),
        agent: "app".into(),
        progress: &|_| {},
        validate: &|| Ok(()),
        cancel: None,
    })
    .unwrap();
    assert!(report.entries.is_empty(), "{report:?}");
    assert_eq!(report.conflicts.len(), 1);
    assert!(!fx.team.join(".claude/skills/new/SKILL.md").exists());
}

#[test]
fn remote_path_codes_are_recognized_once_and_legacy_errors_stay_unknown() {
    use codesense_pty_core::remote_workspace::{
        WORKSPACE_MISSING, WORKSPACE_OCCUPIED, WORKSPACE_UNSAFE,
    };
    for (code, state, reason) in [
        (
            WORKSPACE_MISSING,
            EditsState::Missing,
            "folder no longer exists on the machine",
        ),
        (
            WORKSPACE_UNSAFE,
            EditsState::Unknown,
            "the workspace path on the machine isn't a normal folder",
        ),
        (
            WORKSPACE_OCCUPIED,
            EditsState::Unknown,
            "something else is at the workspace path",
        ),
    ] {
        let found = WorkspaceEdits::remote_error(format!("{code} remote path cannot be opened"));
        assert_eq!(found.state, state);
        assert_eq!(found.reason.as_deref(), Some(reason));
        assert!(found.files.is_empty());
    }
    assert_eq!(
        WorkspaceEdits::remote_error("remote path is missing, unsafe, or occupied".into()).state,
        EditsState::Unknown
    );
}
