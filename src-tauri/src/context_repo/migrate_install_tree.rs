// SPDX-License-Identifier: MIT
//! SW-CR-8: boot-time fold of the retired `{team}/install/` tree into
//! `{team}/content/`.
//!
//! A context repo that predates the install-tier retirement (SW-XS-14) keeps
//! `{team}/install/.claude/…`, `.bmad/…`, settings, hooks — invisible to the
//! resolver because `install/` is no longer a content layer. [`fold_org_install_tree`]
//! moves those bytes under `content/` on `load_org_cmd` so the resolver sees
//! them, atomically where it can.
//!
//! Idempotent: `install/.folded` marks a completed attempt and short-circuits
//! subsequent loads. File-level conflicts (same relative path exists under both
//! `install/` and `content/`) are left in place and recorded in the
//! [`FoldReport`]; the heal command re-runs ignoring the marker.
//!
//! Scale: a top-level `install/<name>` whose counterpart under `content/` does
//! not exist renames in one syscall (the whole subtree at once); only
//! overlapping directories are walked. The field case was 1,735 files.
//!
//! Does NOT auto-commit — the user reviews the deletions-from-install +
//! additions-under-content pair in `git status` and commits themselves.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::path::{
    CODESENSE_INTERNAL_DIR, INSTALL_DIR, LAYER_MARKER_DIRS, TEAM_CONTENT_DIR, WORKTREES_DIR,
};

/// Marker file written at the install root after a fold pass that left
/// conflicts behind. Its presence short-circuits subsequent boot-time passes
/// so the user is not told the same thing twice; the heal command ignores it.
pub const FOLDED_MARKER: &str = ".folded";

/// Per-team outcome of one fold pass.
#[derive(Debug, Default, Clone)]
pub struct TeamFoldResult {
    pub team: String,
    /// Count of regular files moved from `install/` into `content/`.
    pub moved: usize,
    /// Paths (relative to the team's install root) that exist under both
    /// `install/` and `content/` and were left in place.
    pub conflicts: Vec<InstallConflict>,
    /// `true` if the team's `install/` dir was removed entirely after the pass.
    pub install_removed: bool,
    /// `true` if `.folded` already existed and this pass skipped the team.
    pub skipped_marker: bool,
}

#[derive(Debug, Clone)]
pub struct InstallConflict {
    pub team: String,
    pub relpath: PathBuf,
}

/// The outcome across every team of one org.
#[derive(Debug, Default, Clone)]
pub struct FoldReport {
    pub per_team: Vec<TeamFoldResult>,
}

impl FoldReport {
    pub fn total_moved(&self) -> usize {
        self.per_team.iter().map(|t| t.moved).sum()
    }

    pub fn total_conflicts(&self) -> usize {
        self.per_team.iter().map(|t| t.conflicts.len()).sum()
    }

    /// Nothing was touched: every team either had no `install/` or was
    /// short-circuited by its marker, and no new conflicts surfaced.
    pub fn is_noop(&self) -> bool {
        self.per_team
            .iter()
            .all(|t| t.moved == 0 && t.conflicts.is_empty() && !t.install_removed)
    }
}

/// Fold every team's `install/` into its `content/` under `org_root`.
///
/// Teams are discovered by direct `read_dir` on `org_root` rather than from
/// the registry — a broken `teams.yml` must not block the migration. Well-
/// known non-team siblings (`_codesense`, `worktrees`, dotfiles) are skipped.
///
/// `force=false` is the boot-time pass: a team with `install/.folded` is
/// skipped. `force=true` is the heal action and ignores the marker.
pub fn fold_org_install_tree(org_root: &Path, force: bool) -> io::Result<FoldReport> {
    let mut report = FoldReport::default();
    if !org_root.is_dir() {
        return Ok(report);
    }
    let mut entries: Vec<_> = fs::read_dir(org_root)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let team_dir = entry.path();
        let Some(name) = team_dir.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if !is_team_dir_candidate(name) {
            continue;
        }
        if !team_dir.join(INSTALL_DIR).is_dir() {
            continue;
        }
        match fold_team(&team_dir, name, force) {
            Ok(result) => report.per_team.push(result),
            Err(err) => {
                log::warn!(
                    target: "context_repo::migrate_install_tree",
                    "fold {}: {err}",
                    team_dir.display()
                );
            }
        }
    }
    Ok(report)
}

/// Read-only scan: which teams still have `install/` leftovers that would
/// never be delivered to a launch. Used by the validate finding.
pub fn check_install_leftovers(org_root: &Path) -> Vec<InstallConflict> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(org_root) else {
        return out;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let team_dir = entry.path();
        let Some(name) = team_dir.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if !is_team_dir_candidate(name) {
            continue;
        }
        let install = team_dir.join(INSTALL_DIR);
        if !install.is_dir() {
            continue;
        }
        collect_files(Path::new(""), name, &install, &mut out);
    }
    out
}

fn collect_files(rel: &Path, team: &str, at: &Path, out: &mut Vec<InstallConflict>) {
    let Ok(entries) = fs::read_dir(at) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if rel.as_os_str().is_empty() && name == FOLDED_MARKER {
            continue;
        }
        let child = at.join(&name);
        let child_rel = rel.join(&name);
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            collect_files(&child_rel, team, &child, out);
        } else {
            out.push(InstallConflict {
                team: team.to_string(),
                relpath: child_rel,
            });
        }
    }
}

/// Direct children of `org_root` that look like team dirs. Excluded:
/// well-known reserved siblings (`_codesense`, `worktrees`, `.codesense`,
/// `.git`), the layer-marker names (`content` / `repos` / `libraries` from
/// [`LAYER_MARKER_DIRS`], plus [`INSTALL_DIR`]), and dotfiles. The reservation
/// is the same `validate_team_name_full` enforces (`branch_common.rs`), so a
/// legitimate team can never match a reserved name — but a malformed legacy
/// repo with `{org_root}/content/install/` must not be re-folded as if
/// `content` were a team.
fn is_team_dir_candidate(name: &str) -> bool {
    !matches!(
        name,
        CODESENSE_INTERNAL_DIR | WORKTREES_DIR | ".codesense" | ".git"
    ) && name != INSTALL_DIR
        && !LAYER_MARKER_DIRS.contains(&name)
        && !name.starts_with('.')
}

fn fold_team(team_dir: &Path, team: &str, force: bool) -> io::Result<TeamFoldResult> {
    let install = team_dir.join(INSTALL_DIR);
    let content = team_dir.join(TEAM_CONTENT_DIR);
    let marker = install.join(FOLDED_MARKER);
    if !force && marker.is_file() {
        // If the user manually resolved every conflict after the previous pass
        // (install/ now carries only the marker), sweep it instead of leaving
        // an orphan `install/.folded` for `git status` to flag forever —
        // without force this is the only moment a boot pass sees the final
        // emptied state.
        if is_empty_except_marker(&install)? {
            let _ = fs::remove_file(&marker);
            let _ = fs::remove_dir(&install);
            log::info!(
                target: "context_repo::migrate_install_tree",
                "team={team} swept orphan install/.folded (install dir emptied by hand)",
            );
            return Ok(TeamFoldResult {
                team: team.to_string(),
                install_removed: true,
                ..Default::default()
            });
        }
        return Ok(TeamFoldResult {
            team: team.to_string(),
            skipped_marker: true,
            ..Default::default()
        });
    }
    fs::create_dir_all(&content)?;
    let mut result = TeamFoldResult {
        team: team.to_string(),
        ..Default::default()
    };
    // Clear any stale marker before the pass; cleanup_or_mark will rewrite it
    // if leftovers remain.
    let _ = fs::remove_file(&marker);
    merge_dir(&install, &content, Path::new(""), team, &mut result)?;
    cleanup_or_mark(&install, &mut result)?;
    log::info!(
        target: "context_repo::migrate_install_tree",
        "team={team} moved={} conflicts={} install_removed={}",
        result.moved,
        result.conflicts.len(),
        result.install_removed
    );
    Ok(result)
}

/// Recursively merge `src` into `dst`. For a top-level entry whose
/// counterpart under `dst` does not exist, one `rename` moves the whole
/// subtree atomically — the 1,735-file case stays fast.
fn merge_dir(
    src: &Path,
    dst: &Path,
    rel: &Path,
    team: &str,
    result: &mut TeamFoldResult,
) -> io::Result<()> {
    let entries: Vec<_> = fs::read_dir(src)?.filter_map(|e| e.ok()).collect();
    for entry in entries {
        let name = entry.file_name();
        if rel.as_os_str().is_empty() && name == FOLDED_MARKER {
            continue;
        }
        let src_child = src.join(&name);
        let dst_child = dst.join(&name);
        let child_rel = rel.join(&name);
        let ft = entry.file_type()?;
        // `symlink_metadata` sees a dangling symlink that `exists()` would miss
        // and let `rename` silently clobber; it also sees any other kind of
        // entry we must not overwrite.
        if fs::symlink_metadata(&dst_child).is_err() {
            let count = if ft.is_dir() {
                count_files(&src_child)?
            } else {
                1
            };
            fs::rename(&src_child, &dst_child)?;
            result.moved += count;
            continue;
        }
        if ft.is_dir() && dst_child.is_dir() {
            merge_dir(&src_child, &dst_child, &child_rel, team, result)?;
            // The subdir only removes if the recursion emptied it.
            let _ = fs::remove_dir(&src_child);
            continue;
        }
        result.conflicts.push(InstallConflict {
            team: team.to_string(),
            relpath: child_rel,
        });
    }
    Ok(())
}

fn count_files(path: &Path) -> io::Result<usize> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Ok(1);
    }
    let mut n = 0;
    for entry in fs::read_dir(path)?.flatten() {
        n += count_files(&entry.path())?;
    }
    Ok(n)
}

/// Whether `install/` contains nothing but the `.folded` marker (or no marker
/// at all). Used by the marker short-circuit in `fold_team` to sweep an orphan
/// marker left behind when the user manually cleared the conflicts.
fn is_empty_except_marker(install: &Path) -> io::Result<bool> {
    for entry in fs::read_dir(install)? {
        let entry = entry?;
        if entry.file_name() != FOLDED_MARKER {
            return Ok(false);
        }
    }
    Ok(true)
}

fn cleanup_or_mark(install: &Path, result: &mut TeamFoldResult) -> io::Result<()> {
    let has_leftovers = fs::read_dir(install)?
        .filter_map(|e| e.ok())
        .any(|e| e.file_name() != FOLDED_MARKER);
    if has_leftovers {
        fs::write(install.join(FOLDED_MARKER), b"")?;
        return Ok(());
    }
    let marker = install.join(FOLDED_MARKER);
    if marker.is_file() {
        let _ = fs::remove_file(marker);
    }
    fs::remove_dir(install)?;
    result.install_removed = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = fs::File::create(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, bytes: &[u8]) {
        write(path, bytes);
        let mut p = fs::metadata(path).unwrap().permissions();
        p.set_mode(0o755);
        fs::set_permissions(path, p).unwrap();
    }

    fn seed_team(org_root: &Path, team: &str) -> PathBuf {
        let team_dir = org_root.join(team);
        fs::create_dir_all(team_dir.join(INSTALL_DIR)).unwrap();
        fs::create_dir_all(team_dir.join(TEAM_CONTENT_DIR)).unwrap();
        team_dir
    }

    /// AC-1: a fresh team with `install/.claude/hooks/notify.sh`,
    /// `install/.bmad/skill.md`, `install/settings.json` moves them under
    /// `content/`, preserves the exec bit, and removes an empty `install/`.
    #[test]
    #[cfg(unix)]
    fn ac1_fresh_fold_moves_files_preserves_exec_bit_and_removes_install() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed_team(tmp.path(), "default");
        write_executable(
            &team.join("install/.claude/hooks/notify.sh"),
            b"#!/bin/sh\n",
        );
        write(&team.join("install/.bmad/skill.md"), b"# skill\n");
        write(&team.join("install/settings.json"), b"{}\n");

        let report = fold_org_install_tree(tmp.path(), false).unwrap();
        assert_eq!(report.total_moved(), 3);
        assert_eq!(report.total_conflicts(), 0);
        assert!(report.per_team[0].install_removed);

        let hook = team.join("content/.claude/hooks/notify.sh");
        assert!(hook.is_file());
        let mode = fs::metadata(&hook).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o111, 0o111, "exec bit preserved, got {mode:o}");
        assert!(team.join("content/.bmad/skill.md").is_file());
        assert!(team.join("content/settings.json").is_file());
        assert!(!team.join("install").exists(), "install dir removed");
    }

    /// AC-2: a file that exists in both `install/` and `content/` with
    /// different bytes is a conflict — both are left in place and the
    /// finding names the path.
    #[test]
    fn ac2_conflict_leaves_both_files_and_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed_team(tmp.path(), "default");
        write(&team.join("install/.claude/hooks/notify.sh"), b"old\n");
        write(&team.join("content/.claude/hooks/notify.sh"), b"new\n");

        let report = fold_org_install_tree(tmp.path(), false).unwrap();
        assert_eq!(report.total_conflicts(), 1);
        let c = &report.per_team[0].conflicts[0];
        assert_eq!(c.team, "default");
        assert_eq!(c.relpath, PathBuf::from(".claude/hooks/notify.sh"));
        assert_eq!(
            fs::read(team.join("install/.claude/hooks/notify.sh")).unwrap(),
            b"old\n"
        );
        assert_eq!(
            fs::read(team.join("content/.claude/hooks/notify.sh")).unwrap(),
            b"new\n"
        );
        assert!(
            team.join("install/.folded").is_file(),
            "marker written on leftover"
        );
    }

    /// AC-3: a second pass is a no-op (marker short-circuits).
    #[test]
    fn ac3_idempotent_second_pass_does_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed_team(tmp.path(), "default");
        write(&team.join("install/x.md"), b"x\n");
        write(&team.join("content/x.md"), b"y\n"); // force a conflict → marker

        let r1 = fold_org_install_tree(tmp.path(), false).unwrap();
        assert_eq!(r1.total_conflicts(), 1);
        assert!(team.join("install/.folded").is_file());

        let r2 = fold_org_install_tree(tmp.path(), false).unwrap();
        assert!(r2.per_team[0].skipped_marker);
        assert_eq!(r2.total_moved(), 0);
        assert_eq!(r2.total_conflicts(), 0);
    }

    /// Heal (force) ignores the marker and re-runs.
    #[test]
    fn force_pass_ignores_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed_team(tmp.path(), "default");
        write(&team.join("install/x.md"), b"x\n");
        write(&team.join("install/.folded"), b"");

        let report = fold_org_install_tree(tmp.path(), true).unwrap();
        assert_eq!(report.total_moved(), 1);
        assert!(team.join("content/x.md").is_file());
        assert!(!team.join("install").exists());
    }

    /// AC-7: a team without `install/` is a no-op; the dir is not created.
    #[test]
    fn ac7_no_regression_without_install_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed_team(tmp.path(), "default");
        fs::remove_dir(team.join(INSTALL_DIR)).unwrap();
        write(&team.join("content/README.md"), b"# hello\n");

        let report = fold_org_install_tree(tmp.path(), false).unwrap();
        assert!(report.is_noop());
        assert!(report.per_team.is_empty());
        assert_eq!(
            fs::read(team.join("content/README.md")).unwrap(),
            b"# hello\n"
        );
    }

    /// A reserved sibling dir of `org_root` is never traversed as a team.
    #[test]
    fn reserved_siblings_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("_codesense/install")).unwrap();
        fs::create_dir_all(tmp.path().join("worktrees/install")).unwrap();
        fs::create_dir_all(tmp.path().join(".codesense/install")).unwrap();
        let report = fold_org_install_tree(tmp.path(), false).unwrap();
        assert!(report.per_team.is_empty());
        assert!(tmp.path().join("_codesense/install").is_dir());
    }

    /// R2 NIT 1: a malformed legacy repo whose org_root carries a top-level
    /// layer-marker name (`content` / `repos` / `libraries`) OR a top-level
    /// `install/` is never folded as if it were a team.
    /// `validate_team_name_full` rejects these as team names, so a legitimate
    /// repo never has them at the org root — but we must not re-fold a bad
    /// `{org_root}/content/install/` → `{org_root}/content/content/`.
    #[test]
    fn layer_marker_and_install_names_never_treated_as_teams() {
        let tmp = tempfile::tempdir().unwrap();
        for reserved in ["content", "repos", "libraries", "install"] {
            fs::create_dir_all(tmp.path().join(reserved).join("install")).unwrap();
            write(
                &tmp.path().join(reserved).join("install").join("x.md"),
                b"nope",
            );
        }
        let report = fold_org_install_tree(tmp.path(), false).unwrap();
        assert!(
            report.per_team.is_empty(),
            "no reserved sibling folded, got {report:?}"
        );
        for reserved in ["content", "repos", "libraries", "install"] {
            assert!(
                tmp.path().join(reserved).join("install/x.md").is_file(),
                "{reserved}/install/x.md untouched"
            );
        }
    }

    /// R2 NIT 3: the user manually resolved every conflict after the first
    /// pass (install/ now carries only the `.folded` marker). The next boot
    /// pass sweeps the orphan marker + empty install dir instead of leaving
    /// `install/.folded` lingering in `git status`.
    #[test]
    fn orphan_folded_marker_swept_after_manual_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed_team(tmp.path(), "default");
        // Simulate the state after a conflict-only pass whose leftover was
        // manually resolved: install/ contains only `.folded`.
        write(&team.join("install/.folded"), b"");

        let report = fold_org_install_tree(tmp.path(), false).unwrap();
        assert!(report.per_team[0].install_removed);
        assert!(!report.per_team[0].skipped_marker);
        assert!(!team.join("install").exists(), "stale install/ swept");
    }

    /// R2 NIT 5 regression: a dangling symlink at `content/<name>` must NOT
    /// be silently clobbered by the fold's `rename`. The dangling symlink
    /// takes the `exists`-false path with the naive check; `symlink_metadata`
    /// catches it and we record a conflict.
    #[test]
    #[cfg(unix)]
    fn dangling_symlink_at_content_is_a_conflict_not_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed_team(tmp.path(), "default");
        write(&team.join("install/x.md"), b"install-bytes");
        std::os::unix::fs::symlink(tmp.path().join("nowhere.md"), team.join("content/x.md"))
            .unwrap();

        let report = fold_org_install_tree(tmp.path(), false).unwrap();
        assert_eq!(report.total_moved(), 0);
        assert_eq!(report.total_conflicts(), 1);
        assert!(
            team.join("install/x.md").is_file(),
            "install/x.md left intact"
        );
        assert!(
            fs::symlink_metadata(team.join("content/x.md"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "content/x.md symlink untouched"
        );
    }

    /// Scale: a top-level dir with 1,000 files whose counterpart under
    /// `content/` does not exist moves in one syscall (count check + no
    /// per-file collisions).
    #[test]
    fn large_subtree_moves_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed_team(tmp.path(), "default");
        let large = team.join("install/_bmad/skills");
        fs::create_dir_all(&large).unwrap();
        for i in 0..1_000 {
            write(&large.join(format!("s{i}.md")), b"x");
        }
        let report = fold_org_install_tree(tmp.path(), false).unwrap();
        assert_eq!(report.total_moved(), 1_000);
        assert!(team.join("content/_bmad/skills/s500.md").is_file());
        assert!(!team.join("install").exists());
    }

    /// Overlapping dirs merge per file; non-overlapping files move atomically.
    #[test]
    fn overlapping_dirs_merge_per_file() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed_team(tmp.path(), "default");
        write(&team.join("install/.claude/a.md"), b"A\n");
        write(&team.join("install/.claude/b.md"), b"B\n");
        write(&team.join("content/.claude/c.md"), b"C\n");

        let report = fold_org_install_tree(tmp.path(), false).unwrap();
        assert_eq!(report.total_moved(), 2);
        assert_eq!(report.total_conflicts(), 0);
        assert!(team.join("content/.claude/a.md").is_file());
        assert!(team.join("content/.claude/b.md").is_file());
        assert!(team.join("content/.claude/c.md").is_file());
        assert!(!team.join("install").exists());
    }

    /// Multiple teams in one org all fold.
    #[test]
    fn multi_team_org_folds_each() {
        let tmp = tempfile::tempdir().unwrap();
        for team in ["default", "mobile", "web"] {
            let dir = seed_team(tmp.path(), team);
            write(&dir.join("install/x.md"), format!("{team}\n").as_bytes());
        }
        let report = fold_org_install_tree(tmp.path(), false).unwrap();
        assert_eq!(report.per_team.len(), 3);
        assert_eq!(report.total_moved(), 3);
        for team in ["default", "mobile", "web"] {
            assert!(tmp.path().join(team).join("content/x.md").is_file());
            assert!(!tmp.path().join(team).join("install").exists());
        }
    }

    /// A dir-vs-file collision (install has a file where content has a dir of
    /// the same name) is a conflict and both are left in place.
    #[test]
    fn dir_vs_file_collision_is_a_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed_team(tmp.path(), "default");
        write(&team.join("install/.claude"), b"i am a file\n");
        write(&team.join("content/.claude/a.md"), b"A\n");

        let report = fold_org_install_tree(tmp.path(), false).unwrap();
        assert_eq!(report.total_conflicts(), 1);
        assert!(team.join("install/.claude").is_file());
        assert!(team.join("content/.claude/a.md").is_file());
    }

    /// Leftovers after a pass are visible to the read-only scan the validate
    /// finding uses.
    #[test]
    fn check_leftovers_lists_remaining_files() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed_team(tmp.path(), "default");
        write(&team.join("install/x.md"), b"x\n");
        write(&team.join("content/x.md"), b"y\n");
        fold_org_install_tree(tmp.path(), false).unwrap();
        let leftovers = check_install_leftovers(tmp.path());
        assert_eq!(leftovers.len(), 1);
        assert_eq!(leftovers[0].team, "default");
        assert_eq!(leftovers[0].relpath, PathBuf::from("x.md"));
    }
}
