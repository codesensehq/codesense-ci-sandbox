// SPDX-License-Identifier: MIT
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::path::default_skeleton_dirs;

// ---------- Seed types ----------

/// Outcome of seeding a tracked repo's content directory.
#[derive(Debug, Clone)]
pub enum SeedOutcome {
    /// Directories were created and `.gitkeep` files written.
    /// `gitkeep_paths` contains paths **relative to `repo_content_root`**
    /// (e.g., `_bmad-output/planning-artifacts/.gitkeep`).
    Seeded { gitkeep_paths: Vec<PathBuf> },
    /// All skeleton directories already existed — no-op.
    AlreadySeeded,
}

/// Errors returned by [`seed_repo_content`].
#[derive(Debug)]
pub enum SeedError {
    /// The `repo_content_root` directory does not exist.
    RootMissing { path: PathBuf },
    /// A filesystem operation (mkdir, write) failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for SeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeedError::RootMissing { path } => {
                write!(f, "repo content root does not exist: {}", path.display())
            }
            SeedError::Io { path, source } => {
                write!(f, "filesystem error at {}: {}", path.display(), source)
            }
        }
    }
}

impl std::error::Error for SeedError {}

// ---------- seed_repo_content ----------

/// Seed a tracked repo's content directory with skeleton directories.
///
/// Creates each directory in `skeleton_dirs` under `repo_content_root`,
/// each with a `.gitkeep` file. Idempotent: skips directories that
/// already have any file inside them.
///
/// Does **not** commit — the caller stages and commits.
///
/// # Arguments
/// - `repo_content_root`: absolute path to
///   `{org_root}/{team}/repos/{owner}/{repo}/`
/// - `skeleton_dirs`: directory paths to create (relative to root)
pub fn seed_repo_content(
    repo_content_root: &Path,
    skeleton_dirs: &[&str],
) -> Result<SeedOutcome, SeedError> {
    if !repo_content_root.is_dir() {
        return Err(SeedError::RootMissing {
            path: repo_content_root.to_path_buf(),
        });
    }

    let mut created = Vec::new();

    for dir in skeleton_dirs {
        let abs_dir = repo_content_root.join(dir);
        let gitkeep = abs_dir.join(".gitkeep");
        let rel_gitkeep = PathBuf::from(dir).join(".gitkeep");

        // If the directory already exists and has any file, skip.
        if abs_dir.is_dir() && dir_has_any_file(&abs_dir) {
            continue;
        }

        // Create the directory (and parents).
        std::fs::create_dir_all(&abs_dir).map_err(|source| SeedError::Io {
            path: abs_dir.clone(),
            source,
        })?;

        // Write .gitkeep only if it doesn't exist and no other file is present.
        if !dir_has_any_file(&abs_dir) {
            std::fs::write(&gitkeep, "").map_err(|source| SeedError::Io {
                path: gitkeep,
                source,
            })?;
            created.push(rel_gitkeep);
        }
    }

    if created.is_empty() {
        Ok(SeedOutcome::AlreadySeeded)
    } else {
        Ok(SeedOutcome::Seeded {
            gitkeep_paths: created,
        })
    }
}

/// Returns `true` if `dir` contains at least one file (not subdirectory).
fn dir_has_any_file(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .any(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false)),
        Err(_) => false,
    }
}

// ---------- Bootstrap types ----------

/// Result of [`bootstrap_workspace_content`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapResult {
    pub repos_checked: usize,
    pub repos_seeded: usize,
    pub already_complete: usize,
    pub errors: Vec<String>,
}

/// Errors returned by [`bootstrap_workspace_content`].
#[derive(Debug)]
pub enum BootstrapError {
    /// Failed to read `teams.yml`.
    TeamsYmlRead(String),
    /// Failed to parse `teams.yml`.
    TeamsYmlParse(String),
    /// Git operation failed.
    Git(String),
}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootstrapError::TeamsYmlRead(e) => write!(f, "read teams.yml: {e}"),
            BootstrapError::TeamsYmlParse(e) => write!(f, "parse teams.yml: {e}"),
            BootstrapError::Git(e) => write!(f, "git: {e}"),
        }
    }
}

impl std::error::Error for BootstrapError {}

// ---------- bootstrap_workspace_content ----------

/// Iterate all tracked repos in an org's `teams.yml`, seed any missing
/// skeleton directories, and commit the changes (if any).
///
/// Acquires `BRANCH_LOCK` for the git add + commit sequence.
/// Commits locally — does not push.
pub fn bootstrap_workspace_content(org_root: &Path) -> Result<BootstrapResult, BootstrapError> {
    use crate::domain::parse_teams_yml;

    // Validate org_root is a git repo before doing any filesystem writes.
    if !org_root.join(".git").exists() {
        return Err(BootstrapError::Git(format!(
            "not a git repository: {}",
            org_root.display()
        )));
    }

    let teams_yml_path = org_root.join("teams.yml");
    let teams_bytes = std::fs::read(&teams_yml_path)
        .map_err(|e| BootstrapError::TeamsYmlRead(format!("{}: {e}", teams_yml_path.display())))?;
    let raw =
        parse_teams_yml(&teams_bytes).map_err(|e| BootstrapError::TeamsYmlParse(e.to_string()))?;

    let mut repos_checked: usize = 0;
    let mut repos_seeded: usize = 0;
    let mut already_complete: usize = 0;
    let mut errors: Vec<String> = Vec::new();

    for team in &raw.teams {
        for sr in &team.source_repos {
            repos_checked += 1;
            let root = crate::domain::SourceRepoSlug::with_provider_and_host(
                sr.owner.clone(),
                sr.repo.clone(),
                sr.provider.clone(),
                sr.host.clone(),
            )
            .map_err(|e| e.to_string())
            .and_then(|slug| {
                super::layout_resolver::resolve_repository_root(org_root, &team.name, &slug)
            });
            let repo_content_root = match root {
                Ok(root) => root,
                Err(error) => {
                    errors.push(error);
                    continue;
                }
            };

            // Create bare dir if missing (repair handles this too).
            if let Err(e) = std::fs::create_dir_all(&repo_content_root) {
                errors.push(format!("{}/{}: create dir: {e}", sr.owner, sr.repo));
                continue;
            }

            match seed_repo_content(&repo_content_root, default_skeleton_dirs()) {
                Ok(SeedOutcome::Seeded { .. }) => {
                    repos_seeded += 1;
                }
                Ok(SeedOutcome::AlreadySeeded) => {
                    already_complete += 1;
                }
                Err(e) => {
                    errors.push(format!("{}/{}: {e}", sr.owner, sr.repo));
                }
            }
        }
    }

    Ok(BootstrapResult {
        repos_checked,
        repos_seeded,
        already_complete,
        errors,
    })
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Helper: init a bare git repo with one commit on `refs/heads/main`
    /// and a `teams.yml` file seeded with the given YAML content.
    fn init_repo_with_teams_yml(dir: &Path, teams_yml_content: &str) {
        let repo = git2::Repository::init(dir).unwrap();
        // Write teams.yml to the working tree.
        std::fs::write(dir.join("teams.yml"), teams_yml_content).unwrap();
        // Stage and commit.
        let mut index = repo.index().unwrap();
        index.add_path(&PathBuf::from("teams.yml")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }

    #[test]
    fn seed_empty_dir_creates_all_skeleton_dirs() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();

        let result = seed_repo_content(&root, default_skeleton_dirs()).unwrap();

        match &result {
            SeedOutcome::Seeded { gitkeep_paths } => {
                assert_eq!(gitkeep_paths.len(), 3);
                assert!(gitkeep_paths
                    .contains(&PathBuf::from("_bmad-output/planning-artifacts/.gitkeep")));
                assert!(gitkeep_paths.contains(&PathBuf::from(
                    "_bmad-output/implementation-artifacts/.gitkeep"
                )));
                assert!(
                    gitkeep_paths.contains(&PathBuf::from("_bmad-output/test-artifacts/.gitkeep"))
                );
            }
            SeedOutcome::AlreadySeeded => panic!("expected Seeded, got AlreadySeeded"),
        }

        // Verify files exist on disk.
        for dir in default_skeleton_dirs() {
            let gitkeep = root.join(dir).join(".gitkeep");
            assert!(gitkeep.exists(), "missing .gitkeep in {}", dir);
        }
    }

    #[test]
    fn seed_already_seeded_returns_already_seeded() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();

        // First seed.
        seed_repo_content(&root, default_skeleton_dirs()).unwrap();

        // Second seed — should be no-op.
        let result = seed_repo_content(&root, default_skeleton_dirs()).unwrap();
        assert!(matches!(result, SeedOutcome::AlreadySeeded));
    }

    #[test]
    fn seed_partially_seeded_creates_only_missing() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();

        // Pre-create one skeleton dir with .gitkeep.
        let pre = root.join("_bmad-output/planning-artifacts");
        std::fs::create_dir_all(&pre).unwrap();
        std::fs::write(pre.join(".gitkeep"), "").unwrap();

        let result = seed_repo_content(&root, default_skeleton_dirs()).unwrap();

        match &result {
            SeedOutcome::Seeded { gitkeep_paths } => {
                assert_eq!(gitkeep_paths.len(), 2);
                assert!(!gitkeep_paths
                    .contains(&PathBuf::from("_bmad-output/planning-artifacts/.gitkeep")));
            }
            SeedOutcome::AlreadySeeded => panic!("expected Seeded"),
        }
    }

    #[test]
    fn seed_skips_dir_with_user_content() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();

        // Pre-create planning-artifacts with user content (not .gitkeep).
        let user_dir = root.join("_bmad-output/planning-artifacts");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join("my-plan.md"), "# Plan").unwrap();

        let result = seed_repo_content(&root, default_skeleton_dirs()).unwrap();

        match &result {
            SeedOutcome::Seeded { gitkeep_paths } => {
                // Should only create the 2 missing dirs, skipping planning-artifacts.
                assert_eq!(gitkeep_paths.len(), 2);
                assert!(!gitkeep_paths
                    .contains(&PathBuf::from("_bmad-output/planning-artifacts/.gitkeep")));
            }
            SeedOutcome::AlreadySeeded => panic!("expected Seeded"),
        }

        // User content must not be modified.
        let content = std::fs::read_to_string(user_dir.join("my-plan.md")).unwrap();
        assert_eq!(content, "# Plan");
        // No .gitkeep should have been written alongside user content.
        assert!(!user_dir.join(".gitkeep").exists());
    }

    #[test]
    fn seed_errors_when_root_missing() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("nonexistent");

        let err = seed_repo_content(&root, default_skeleton_dirs()).unwrap_err();
        assert!(matches!(err, SeedError::RootMissing { .. }));
    }

    // ---- bootstrap_workspace_content tests ----

    #[test]
    fn bootstrap_seeds_two_tracked_repos() {
        let tmp = tempdir().unwrap();
        let org = tmp.path();
        init_repo_with_teams_yml(
            org,
            "teams:\n  - name: default\n    source_repos:\n      - owner: acme\n        repo: api\n      - owner: acme\n        repo: web\n",
        );

        let result = bootstrap_workspace_content(org).unwrap();
        assert_eq!(result.repos_checked, 2);
        assert_eq!(result.repos_seeded, 2);
        assert_eq!(result.already_complete, 0);
        assert!(result.errors.is_empty());

        // Verify .gitkeep files exist.
        for repo_name in &["api", "web"] {
            for dir in default_skeleton_dirs() {
                let gk = org
                    .join("default/repos/github.com/acme")
                    .join(repo_name)
                    .join(dir)
                    .join(".gitkeep");
                assert!(gk.exists(), "missing {}", gk.display());
            }
        }

        // Bootstrap leaves the existing history unchanged.
        let repo = git2::Repository::open(org).unwrap();
        let main = repo
            .find_reference("refs/heads/main")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert_eq!(main.message().unwrap(), "init");
    }

    #[test]
    fn bootstrap_one_already_seeded_seeds_only_missing() {
        let tmp = tempdir().unwrap();
        let org = tmp.path();
        init_repo_with_teams_yml(
            org,
            "teams:\n  - name: default\n    source_repos:\n      - owner: acme\n        repo: api\n      - owner: acme\n        repo: web\n",
        );

        // Pre-seed one repo.
        let api_root = org.join("default/repos/acme/api");
        std::fs::create_dir_all(&api_root).unwrap();
        seed_repo_content(&api_root, default_skeleton_dirs()).unwrap();

        let result = bootstrap_workspace_content(org).unwrap();
        assert_eq!(result.repos_checked, 2);
        assert_eq!(result.repos_seeded, 1);
        assert_eq!(result.already_complete, 1);
    }

    #[test]
    fn bootstrap_all_seeded_no_commit() {
        let tmp = tempdir().unwrap();
        let org = tmp.path();
        init_repo_with_teams_yml(
            org,
            "teams:\n  - name: default\n    source_repos:\n      - owner: acme\n        repo: api\n",
        );

        // Pre-seed the only repo.
        let api_root = org.join("default/repos/acme/api");
        std::fs::create_dir_all(&api_root).unwrap();
        seed_repo_content(&api_root, default_skeleton_dirs()).unwrap();

        let result = bootstrap_workspace_content(org).unwrap();
        assert_eq!(result.repos_checked, 1);
        assert_eq!(result.repos_seeded, 0);
        assert_eq!(result.already_complete, 1);

        // Only the init commit should exist (no bootstrap commit).
        let repo = git2::Repository::open(org).unwrap();
        let main = repo
            .find_reference("refs/heads/main")
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert_eq!(main.message().unwrap(), "init");
    }

    #[test]
    fn bootstrap_no_tracked_repos_is_noop() {
        let tmp = tempdir().unwrap();
        let org = tmp.path();
        init_repo_with_teams_yml(org, "teams:\n  - name: default\n    source_repos: []\n");

        let result = bootstrap_workspace_content(org).unwrap();
        assert_eq!(result.repos_checked, 0);
        assert_eq!(result.repos_seeded, 0);
        assert_eq!(result.already_complete, 0);
    }
}
