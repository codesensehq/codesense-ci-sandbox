// SPDX-License-Identifier: MIT
//! Team rename orchestration (SW-HB-8).
//!
//! Unlike every other `RegistryEdit`, a team rename is not a pure
//! `(path, bytes)` file-write — the team name is a **path component** as well
//! as a registry field, so a rename MOVES directories:
//!
//! - `teams.yml` — the team's `name` field (git-tracked, inside `org_root`).
//! - `{name}/` — the whole team subtree, flat at the repo root (git-tracked,
//!   inside `org_root`; SLW-IN-7 dropped the `_team/` wrapper).
//! - `installs/{org}/{name}/…` — install lockfiles (NOT git-tracked, a sibling
//!   of `orgs/` outside the context-repo clone).
//!
//! All edits are filesystem operations. The rollback ledger restores only
//! paths changed by this transaction; Git refs, the index and unrelated
//! dirty files remain untouched.

use std::path::{Path, PathBuf};

use super::branch_common::BRANCH_LOCK;
use super::mutate::{
    pr_copy, read_teams_yml, team_not_found, validate_team_name_full, EditContext, RegistryEdit,
    RegistryEditError,
};
use super::path::path_to_install_root;
use crate::domain::Principal;
use crate::domain::RegistryFile;

/// Outcome of a successful `rename_team`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RenameTeamOutcome {
    pub commit_message: String,
}

// Test-only fault flag: when set, `rename_team` injects a failure right after
// the git commit lands but before any non-git move — driving the rollback path
// without hacking the filesystem out from under the orchestrator. A natural
// mid-sequence failure can't be constructed because the pre-flight already
// refuses every occupied destination.
#[cfg(test)]
thread_local! {
    static FAULT_BEFORE_NON_GIT_MOVES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A single filesystem move recorded for rollback. `reverse()` moves the path
/// back from `to` to `from` (best-effort).
struct MoveRecord {
    from: PathBuf,
    to: PathBuf,
}

/// Rename a team in place. See the module doc for the moved surfaces and the
/// atomicity contract. Reuses the shared name validator (`validate_team_name_full`)
/// and `teams.yml` reader from `mutate.rs` — no re-derived rules or literals.
#[allow(clippy::too_many_arguments)]
pub fn rename_team(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    org_root: &Path,
    old_name: &str,
    new_name: &str,
) -> Result<RenameTeamOutcome, RegistryEditError> {
    // ---- Phase A: cheap, side-effect-free arg checks (fail = nothing done) ----

    // The new name obeys the SAME rules as create (slug + `-meta` + length).
    // These depend only on the argument, so run them before taking the lock.
    validate_team_name_full(new_name)?;

    if old_name == new_name {
        return Err(RegistryEditError::Parse(
            "new team name must differ from the current name".to_string(),
        ));
    }

    // ---- Phase B: everything that reads or writes shared registry state runs
    // UNDER `BRANCH_LOCK` ----
    //
    // Acquire the lock BEFORE reading teams.yml so the read → validate →
    // collision-check → commit sequence is atomic against every other registry
    // edit (all of which serialize on the same `BRANCH_LOCK` via
    // `write_edit_to_workdir`). If we read outside the lock, a concurrent
    // CreateTeam / AddSourceRepo could write between our read and our commit, and
    // our whole-file re-serialize would silently overwrite it (lost update).
    let _lock = BRANCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // The team must exist; its target name must be free. Reuse the shared
    // reader so parse/IO errors map to the same variants as every other edit.
    let mut teams = read_teams_yml(org_root)?;
    let old_idx = teams
        .teams
        .iter()
        .position(|t| t.name == old_name)
        .ok_or_else(|| team_not_found(old_name))?;
    if teams.teams.iter().any(|t| t.name == new_name) {
        return Err(RegistryEditError::Duplicate(format!(
            "team '{new_name}' already exists"
        )));
    }

    // Path-collision pre-flight (AC-6a): refuse if any destination path is
    // already occupied — a stale leftover or a race — before we move anything.
    let team_src = org_root.join(old_name);
    let team_dst = org_root.join(new_name);
    if team_dst.exists() {
        return Err(RegistryEditError::Io(format!(
            "destination content dir already exists: {}",
            team_dst.display()
        )));
    }
    let installs_src = path_to_install_root(codesense_home, principal)
        .join(org_slug)
        .join(old_name);
    let installs_dst = path_to_install_root(codesense_home, principal)
        .join(org_slug)
        .join(new_name);
    if installs_dst.exists() {
        return Err(RegistryEditError::Io(format!(
            "destination install dir already exists: {}",
            installs_dst.display()
        )));
    }

    // Capture the original state needed for a full rollback BEFORE any mutation.
    let teams_yml_path = org_root.join(RegistryFile::TeamsYml.filename());
    let original_teams_bytes = std::fs::read(&teams_yml_path)
        .map_err(|e| RegistryEditError::Io(format!("read teams.yml: {e}")))?;

    // Rewrite the entry in place — order + all other fields preserved (AC-2).
    teams.teams[old_idx].name = new_name.to_string();
    // SLW-IN-5 AC-8: `parent_team` is a name-as-foreign-key, so re-point every
    // child that inherits from the old name in the SAME commit — otherwise a
    // rename silently orphans the chain (dangling parent). Rides the existing
    // whole-file serialize + `BRANCH_LOCK` commit + rollback below; no extra
    // commit, no new command.
    for team in teams.teams.iter_mut() {
        if team.parent_team.as_deref() == Some(old_name) {
            team.parent_team = Some(new_name.to_string());
        }
    }
    let new_teams_bytes = serde_yaml::to_string(&teams)
        .map_err(|e| RegistryEditError::Parse(format!("serialize teams.yml: {e}")))?
        .into_bytes();

    let copy = pr_copy(
        &RegistryEdit::RenameTeam {
            old_name: old_name.to_string(),
            new_name: new_name.to_string(),
        },
        &EditContext {
            old_display_name: None,
            target_username: None,
            target_display_name: None,
        },
    );

    // Only filesystem changes participate in this transaction; Git remains user-owned.
    let rollback = |non_git_moves: &[MoveRecord], team_moved: bool| -> bool {
        let mut ok = true;
        for m in non_git_moves.iter().rev() {
            ok &= reverse_move(m);
        }
        if team_moved {
            ok &= reverse_move(&MoveRecord {
                from: team_src.clone(),
                to: team_dst.clone(),
            });
        }
        if let Err(e) = std::fs::write(&teams_yml_path, &original_teams_bytes) {
            log::error!("rename_team rollback: restore teams.yml failed: {e}");
            ok = false;
        }
        ok
    };

    // Move the content subtree on the working tree (atomic same-fs rename).
    // A team with no content dir yet (never scaffolded) is tolerated: skip.
    let team_moved = team_src.exists();
    if team_moved {
        if let Err(e) = std::fs::rename(&team_src, &team_dst) {
            return Err(RegistryEditError::Io(format!(
                "move {} -> {}: {e}",
                team_src.display(),
                team_dst.display()
            )));
        }
    }

    if let Err(e) = std::fs::write(&teams_yml_path, &new_teams_bytes)
        .map_err(|e| RegistryEditError::Io(format!("write teams.yml: {e}")))
    {
        let reverted = rollback(&[], team_moved);
        return Err(finalize_err(e, reverted));
    }

    #[cfg(test)]
    if FAULT_BEFORE_NON_GIT_MOVES.with(|c| c.get()) {
        let reverted = rollback(&[], team_moved);
        return Err(finalize_err(
            RegistryEditError::Io("injected fault: before non-git moves".to_string()),
            reverted,
        ));
    }

    // ---- Phase C: non-git moves last (revert = reverse + git reset) ----

    let mut moves: Vec<MoveRecord> = Vec::new();

    // Install lockfiles (outside git). Absent → no-op (AC-4).
    if installs_src.exists() {
        if let Err(e) = std::fs::rename(&installs_src, &installs_dst) {
            let err = RegistryEditError::Io(format!(
                "move {} -> {}: {e}",
                installs_src.display(),
                installs_dst.display()
            ));
            let reverted = rollback(&moves, team_moved);
            return Err(finalize_err(err, reverted));
        }
        moves.push(MoveRecord {
            from: installs_src.clone(),
            to: installs_dst.clone(),
        });
    }

    Ok(RenameTeamOutcome {
        commit_message: copy.commit_title,
    })
}

/// Reverse a single recorded move. Returns `true` if the source is back in
/// place (either the move-back succeeded, or there was nothing to move back).
/// Used only by rollback.
fn reverse_move(m: &MoveRecord) -> bool {
    if m.to.exists() {
        if let Err(e) = std::fs::rename(&m.to, &m.from) {
            log::error!(
                "rename_team rollback: reverse move {} -> {} failed: {e}",
                m.to.display(),
                m.from.display()
            );
            return false;
        }
    }
    true
}

/// Escalate a rename failure when its rollback did not fully revert. A clean
/// rollback surfaces the ORIGINAL error (the end state is fully-original); an
/// incomplete rollback wraps it so the caller/user knows the state may be
/// partial and needs manual attention — never silently masked.
fn finalize_err(original: RegistryEditError, fully_reverted: bool) -> RegistryEditError {
    if fully_reverted {
        original
    } else {
        RegistryEditError::Io(format!(
            "team rename failed AND rollback was incomplete — on-disk state may be partial; \
             manual recovery may be needed (original error: {original})"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: u64 = 42;
    const ORG: &str = "acme";

    struct Fixture {
        _home: tempfile::TempDir,
        home: PathBuf,
        org_root: PathBuf,
    }

    /// Init a git org repo at `orgs/acme` with a populated `_team/{team}` subtree
    /// and an `installs/acme/{team}/…` lockfile, all under one temp home so the
    /// prod path builders (`path_to_install_root`) resolve correctly.
    fn make_fixture(team: &str) -> Fixture {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path().to_path_buf();
        let org_root = home.join("orgs").join(ORG);
        std::fs::create_dir_all(&org_root).unwrap();

        // The fixture commits to `refs/heads/main`, so HEAD must name it whatever the
        // global `init.defaultBranch` is (unset, or another test's temp HOME, means master).
        let repo = git2::Repository::init_opts(
            &org_root,
            git2::RepositoryInitOptions::new().initial_head("main"),
        )
        .unwrap();
        std::fs::write(
            org_root.join("org.yml"),
            "slug: acme\ndisplay_name: Acme\ncreated_at: 2026-01-01T00:00:00Z\nconfig_flags: {}\n",
        )
        .unwrap();
        std::fs::write(
            org_root.join("people.yml"),
            "people:\n  - github_user_id: 42\n    github_username: creator42\n",
        )
        .unwrap();
        std::fs::write(
            org_root.join("teams.yml"),
            format!(
                "teams:\n  - name: {team}\n    source_repos:\n      - owner: acme\n        repo: web\n  - name: keep\n    source_repos: []\n"
            ),
        )
        .unwrap();
        // Populated content subtree: _team/{team}/content/{epic}/story.md + repos/.gitkeep
        let content = org_root.join(team).join("content").join("epic-1");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::write(content.join("story.md"), "# story\n").unwrap();
        let repos = org_root.join(team).join("repos");
        std::fs::create_dir_all(&repos).unwrap();
        std::fs::write(repos.join(".gitkeep"), "").unwrap();

        // Commit everything to main.
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["."], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // Install lockfile outside git: installs/acme/{team}/owner/repo/hash.lock.json
        let install_dir = path_to_install_root(&home, &Principal::from(UID))
            .join(ORG)
            .join(team)
            .join("owner")
            .join("repo");
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(install_dir.join("abc123.lock.json"), "{}").unwrap();

        Fixture {
            _home: home_dir,
            home,
            org_root,
        }
    }

    fn run(fx: &Fixture, old: &str, new: &str) -> Result<RenameTeamOutcome, RegistryEditError> {
        rename_team(&fx.home, &Principal::from(UID), ORG, &fx.org_root, old, new)
    }

    fn teams_yml(fx: &Fixture) -> String {
        std::fs::read_to_string(fx.org_root.join("teams.yml")).unwrap()
    }

    fn main_commit_count(fx: &Fixture) -> usize {
        let repo = git2::Repository::open(&fx.org_root).unwrap();
        let mut walk = repo.revwalk().unwrap();
        walk.push_ref("refs/heads/main").unwrap();
        walk.count()
    }

    #[test]
    fn happy_path_moves_every_surface_and_renames_in_place() {
        let fx = make_fixture("payments");
        run(&fx, "payments", "billing").unwrap();

        // teams.yml: renamed in place, order + fields preserved.
        let y = teams_yml(&fx);
        assert!(y.contains("name: billing"));
        assert!(!y.contains("name: payments"));
        assert!(y.contains("name: keep"));
        // billing (the renamed first entry) still precedes keep.
        assert!(y.find("name: billing").unwrap() < y.find("name: keep").unwrap());
        // Fields preserved on the renamed entry (source_repos carried through).
        assert!(y.contains("repo: web"));

        // Content subtree moved.
        let base = &fx.org_root;
        assert!(base
            .join("billing")
            .join("content")
            .join("epic-1")
            .join("story.md")
            .is_file());
        assert!(!base.join("payments").exists());

        // Install lockfiles moved (AC-4).
        let installs = path_to_install_root(&fx.home, &Principal::from(UID)).join(ORG);
        assert!(installs
            .join("billing")
            .join("owner")
            .join("repo")
            .join("abc123.lock.json")
            .is_file());
        assert!(!installs.join("payments").exists());

        // One new commit on main.
        assert_eq!(main_commit_count(&fx), 1);
    }

    /// SLW-IN-5 AC-8: renaming a team re-points every child's `parent_team`
    /// (a name-as-foreign-key) in the SAME commit — a rename must never orphan
    /// an inheritance chain.
    #[test]
    fn rename_cascades_parent_team_to_children() {
        let fx = make_fixture("payments");
        // Seed a child inheriting from the soon-to-be-renamed team + a root
        // sibling. rename_team reads teams.yml from disk, so the cascade sees
        // all three even though this overwrite is uncommitted.
        std::fs::write(
            fx.org_root.join("teams.yml"),
            "teams:\n  - name: payments\n    source_repos: []\n  - name: keep\n    source_repos: []\n  - name: sub\n    source_repos: []\n    parent_team: payments\n",
        )
        .unwrap();

        run(&fx, "payments", "billing").unwrap();

        let reparsed = read_teams_yml(&fx.org_root).unwrap();
        let sub = reparsed.teams.iter().find(|t| t.name == "sub").unwrap();
        assert_eq!(
            sub.parent_team,
            Some("billing".to_string()),
            "child's parent_team cascaded payments→billing"
        );
        // The renamed team itself (a root) gains no parent_team key.
        let billing = reparsed.teams.iter().find(|t| t.name == "billing").unwrap();
        assert_eq!(billing.parent_team, None);
    }

    #[test]
    fn missing_old_team_is_not_found() {
        let fx = make_fixture("payments");
        let err = run(&fx, "ghost", "billing").unwrap_err();
        assert!(matches!(err, RegistryEditError::NotFound(_)));
    }

    #[test]
    fn duplicate_new_name_rejected() {
        let fx = make_fixture("payments");
        let err = run(&fx, "payments", "keep").unwrap_err();
        assert!(matches!(err, RegistryEditError::Duplicate(_)));
    }

    #[test]
    fn invalid_and_meta_and_same_name_rejected() {
        let fx = make_fixture("payments");
        assert!(matches!(
            run(&fx, "payments", "Bad Name").unwrap_err(),
            RegistryEditError::Parse(_)
        ));
        assert!(matches!(
            run(&fx, "payments", "payments-meta").unwrap_err(),
            RegistryEditError::Parse(_)
        ));
        assert!(matches!(
            run(&fx, "payments", "payments").unwrap_err(),
            RegistryEditError::Parse(_)
        ));
        // Nothing moved on a rejected rename.
        assert!(fx.org_root.join("payments").exists());
        assert_eq!(main_commit_count(&fx), 1);
    }

    #[test]
    fn preflight_refuses_destination_collision() {
        let fx = make_fixture("payments");
        // Pre-create the destination content dir → collision.
        std::fs::create_dir_all(fx.org_root.join("billing")).unwrap();
        let err = run(&fx, "payments", "billing").unwrap_err();
        assert!(matches!(err, RegistryEditError::Io(_)));
        // Original untouched.
        assert!(fx.org_root.join("payments").exists());
        assert_eq!(main_commit_count(&fx), 1);
    }

    #[test]
    fn absent_install_dir_is_a_noop() {
        let fx = make_fixture("payments");
        // Remove the install tree entirely.
        std::fs::remove_dir_all(path_to_install_root(&fx.home, &Principal::from(UID))).unwrap();
        run(&fx, "payments", "billing").unwrap();
        // Rename still succeeds; content moved, one new commit.
        assert!(fx.org_root.join("billing").exists());
        assert_eq!(main_commit_count(&fx), 1);
    }

    #[test]
    fn rename_preserves_dirty_files_index_and_history_on_success_and_failure() {
        for fail in [false, true] {
            let fx = make_fixture("payments");
            let repo = git2::Repository::open(&fx.org_root).unwrap();
            let dirty = fx.org_root.join("unrelated.txt");
            std::fs::write(&dirty, "staged\n").unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("unrelated.txt")).unwrap();
            index.write().unwrap();
            std::fs::write(&dirty, "unstaged\n").unwrap();
            let before_index = std::fs::read(repo.path().join("index")).unwrap();
            let before_head = repo.head().unwrap().target();
            FAULT_BEFORE_NON_GIT_MOVES.with(|c| c.set(fail));
            let result = run(&fx, "payments", "billing");
            FAULT_BEFORE_NON_GIT_MOVES.with(|c| c.set(false));
            assert_eq!(result.is_err(), fail);
            assert_eq!(repo.head().unwrap().target(), before_head);
            assert_eq!(
                std::fs::read(repo.path().join("index")).unwrap(),
                before_index
            );
            assert_eq!(std::fs::read_to_string(dirty).unwrap(), "unstaged\n");
        }
    }

    #[test]
    fn injected_failure_rolls_back_fully() {
        let fx = make_fixture("payments");
        FAULT_BEFORE_NON_GIT_MOVES.with(|c| c.set(true));
        let err = run(&fx, "payments", "billing").unwrap_err();
        FAULT_BEFORE_NON_GIT_MOVES.with(|c| c.set(false));
        assert!(matches!(err, RegistryEditError::Io(_)));

        // FULL rollback: teams.yml back to original name…
        let y = teams_yml(&fx);
        assert!(y.contains("name: payments"));
        assert!(!y.contains("name: billing"));
        // …content dir back at the old path, new path gone…
        let base = &fx.org_root;
        assert!(base
            .join("payments")
            .join("content")
            .join("epic-1")
            .join("story.md")
            .is_file());
        assert!(!base.join("billing").exists());
        // …installs untouched…
        let installs = path_to_install_root(&fx.home, &Principal::from(UID)).join(ORG);
        assert!(installs.join("payments").exists());
        assert!(!installs.join("billing").exists());
        // …and main back to a single commit (the rename commit was reset away).
        assert_eq!(main_commit_count(&fx), 1);
    }
}
