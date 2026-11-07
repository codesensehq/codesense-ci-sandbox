// SPDX-License-Identifier: MIT
//! SW-CR-8: the Tauri surface for the install-tree fold migration — the
//! read-only `check_install_leftovers_cmd` finding (validate pipeline) and
//! the `heal_install_tree_cmd` one-click re-run (ignores the `.folded`
//! marker). Both share
//! [`crate::context_repo::migrate_install_tree`]; the boot-time pass runs
//! unforced from `load_org_cmd` so the heal command is only needed when
//! conflicts stopped the first pass.

use std::path::PathBuf;

use serde::Serialize;

use crate::context_repo::migrate_install_tree::{
    check_install_leftovers, fold_org_install_tree, FoldReport, InstallConflict, TeamFoldResult,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallLeftoverFinding {
    pub team: String,
    pub relpath: String,
}

impl From<&InstallConflict> for InstallLeftoverFinding {
    fn from(c: &InstallConflict) -> Self {
        Self {
            team: c.team.clone(),
            relpath: c.relpath.to_string_lossy().into_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealResult {
    pub teams: Vec<TeamHealResult>,
    pub total_moved: usize,
    pub total_conflicts: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamHealResult {
    pub team: String,
    pub moved: usize,
    pub install_removed: bool,
    /// `true` if the team's `.folded` marker short-circuited this pass. Only
    /// possible on the boot-time path, since the heal action forces a re-run.
    pub skipped_marker: bool,
    pub conflicts: Vec<InstallLeftoverFinding>,
}

impl From<FoldReport> for HealResult {
    fn from(r: FoldReport) -> Self {
        HealResult {
            total_moved: r.total_moved(),
            total_conflicts: r.total_conflicts(),
            teams: r.per_team.iter().map(TeamHealResult::from).collect(),
        }
    }
}

impl From<&TeamFoldResult> for TeamHealResult {
    fn from(t: &TeamFoldResult) -> Self {
        TeamHealResult {
            team: t.team.clone(),
            moved: t.moved,
            install_removed: t.install_removed,
            skipped_marker: t.skipped_marker,
            conflicts: t
                .conflicts
                .iter()
                .map(InstallLeftoverFinding::from)
                .collect(),
        }
    }
}

fn absolute_org_root(org_root: String) -> Result<PathBuf, String> {
    let path = PathBuf::from(org_root.trim());
    if !path.is_absolute() {
        return Err(format!("org_root must be absolute: {}", path.display()));
    }
    Ok(path)
}

/// Read-only: files still under `{team}/install/` that will not be delivered
/// to a launch. Non-empty ⇒ the validate pipeline surfaces a finding naming
/// each path and offers the heal action.
#[tauri::command]
pub async fn check_install_leftovers_cmd(
    org_root: String,
) -> Result<Vec<InstallLeftoverFinding>, String> {
    crate::commands::await_blocking("check_install_leftovers_cmd", move || {
        let root = absolute_org_root(org_root)?;
        let leftovers = check_install_leftovers(&root)
            .iter()
            .map(InstallLeftoverFinding::from)
            .collect();
        Ok(leftovers)
    })
    .await
}

/// Re-run the fold migration ignoring any `.folded` marker — the one-click
/// heal action. Returns per-team move / conflict counts.
#[tauri::command]
pub async fn heal_install_tree_cmd(org_root: String) -> Result<HealResult, String> {
    crate::commands::await_blocking("heal_install_tree_cmd", move || {
        let root = absolute_org_root(org_root)?;
        let report = fold_org_install_tree(&root, true).map_err(|e| e.to_string())?;
        Ok(HealResult::from(report))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_repo::{INSTALL_DIR, TEAM_CONTENT_DIR};
    use std::fs;

    fn seed(tmp: &std::path::Path, team: &str) -> std::path::PathBuf {
        let dir = tmp.join(team);
        fs::create_dir_all(dir.join(INSTALL_DIR)).unwrap();
        fs::create_dir_all(dir.join(TEAM_CONTENT_DIR)).unwrap();
        dir
    }

    /// AC-4 round-trip: a conflict leftover surfaces in the validate finding,
    /// the heal action re-runs the fold once the conflict is resolved, and
    /// the finding clears afterwards.
    #[tokio::test]
    async fn validate_then_heal_clears_the_finding() {
        let tmp = tempfile::tempdir().unwrap();
        let team = seed(tmp.path(), "default");
        // One leftover: install/y.md vs content/y.md with different bytes —
        // a prior boot-time pass refused the move and left `.folded`.
        fs::write(team.join("install/y.md"), b"i").unwrap();
        fs::write(team.join("content/y.md"), b"ii").unwrap();
        fs::write(team.join("install/.folded"), b"").unwrap();

        let findings = check_install_leftovers_cmd(tmp.path().display().to_string())
            .await
            .unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].team, "default");
        assert_eq!(findings[0].relpath, "y.md");

        // Resolve the conflict (user removes the content copy to accept the
        // install bytes), then heal.
        fs::remove_file(team.join("content/y.md")).unwrap();
        let result = heal_install_tree_cmd(tmp.path().display().to_string())
            .await
            .unwrap();
        assert_eq!(result.total_moved, 1);
        assert_eq!(result.total_conflicts, 0);
        assert!(result.teams[0].install_removed);
        assert!(!result.teams[0].skipped_marker, "force pass ignores marker");

        let after = check_install_leftovers_cmd(tmp.path().display().to_string())
            .await
            .unwrap();
        assert!(after.is_empty());
    }

    #[tokio::test]
    async fn relative_org_root_rejected() {
        let err = check_install_leftovers_cmd("relative/path".into())
            .await
            .unwrap_err();
        assert!(err.contains("absolute"));
        let err = heal_install_tree_cmd("relative/path".into())
            .await
            .unwrap_err();
        assert!(err.contains("absolute"));
    }
}
