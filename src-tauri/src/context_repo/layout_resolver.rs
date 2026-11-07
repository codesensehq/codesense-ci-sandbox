// SPDX-License-Identifier: MIT
//! Two-shape layout resolver for prepared local repository data.
//!
//! A read or write picks between the host-qualified canonical layout and the
//! older hostless legacy layout. Historical migration markers are left on disk
//! and never authorize file selection — only on-disk presence does.
use crate::domain::SourceRepoSlug;
use std::path::{Path, PathBuf};

pub struct LegacyRepositoryPathLocator;

impl LegacyRepositoryPathLocator {
    pub fn under(root: &Path, slug: &SourceRepoSlug) -> PathBuf {
        root.join(&slug.owner).join(&slug.repo)
    }

    pub fn team_repo_leaf(
        org_root: &Path,
        team: &str,
        slug: &SourceRepoSlug,
        leaf: &str,
    ) -> PathBuf {
        Self::under(&org_root.join(team).join(super::REPOS_DIR), slug).join(leaf)
    }

    pub fn worktree_config(org_root: &Path, slug: &SourceRepoSlug) -> PathBuf {
        org_root
            .join(super::CODESENSE_INTERNAL_DIR)
            .join("worktree")
            .join(&slug.owner)
            .join(&slug.repo)
            .with_extension("yaml")
    }
}

/// An existing host-qualified path is the consistent read/write authority.
/// Preserve old copies untouched, including after canonical content is edited.
/// An empty canonical directory also wins so deleting its last file stays deleted.
pub fn resolve_local_layout(canonical: &Path, legacy: &Path) -> Result<PathBuf, String> {
    if exists(canonical)? || !exists(legacy)? {
        Ok(canonical.to_path_buf())
    } else {
        Ok(legacy.to_path_buf())
    }
}

fn exists(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
    }
}

/// Hostless legacy paths predate host-qualified identity. Their registry ownership
/// must be unique before they can supply content or machine-local install state.
pub fn resolve_owned_layout(
    canonical: &Path,
    legacy: &Path,
    org_root: &Path,
    team: Option<&str>,
    slug: &SourceRepoSlug,
) -> Result<PathBuf, String> {
    if exists(canonical)? {
        return Ok(canonical.to_path_buf());
    }
    if exists(legacy)? && !legacy_owner_is_unique(org_root, team, slug)? {
        return Err(format!("Cannot assign hostless legacy repository data at {} to {}: repository ownership is absent or ambiguous across hosts. Prepare the host-qualified path {} with your agent or file tools; no data was moved.", legacy.display(), slug, canonical.display()));
    }
    resolve_local_layout(canonical, legacy)
}

fn legacy_owner_is_unique(
    org_root: &Path,
    team: Option<&str>,
    slug: &SourceRepoSlug,
) -> Result<bool, String> {
    let requested = slug.physical_path_key().map_err(|e| e.to_string())?;
    let bytes = match std::fs::read(org_root.join("teams.yml")) {
        Ok(bytes) => bytes,
        // The original hostless namespace defaulted specifically to github.com.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let cloud = SourceRepoSlug::new(slug.owner.clone(), slug.repo.clone())
                .map_err(|e| e.to_string())?;
            return Ok(requested == cloud.physical_path_key().map_err(|e| e.to_string())?);
        }
        Err(e) => return Err(format!("Cannot inspect legacy repository ownership: {e}")),
    };
    let teams = crate::domain::parse_teams_yml(&bytes).map_err(|e| e.to_string())?;
    let matches = |entry: &&crate::domain::RawSourceRepoEntry| {
        entry.owner.eq_ignore_ascii_case(&slug.owner) && entry.repo.eq_ignore_ascii_case(&slug.repo)
    };
    let scoped: Vec<_> = teams
        .teams
        .iter()
        .filter(|t| team.is_none_or(|name| t.name == name))
        .flat_map(|t| &t.source_repos)
        .filter(matches)
        .collect();
    // An ancestor can provide repo-scoped content while only its child tracks
    // that repo. In this case the org-wide identity must still be unique.
    let candidates: Vec<_> = if scoped.is_empty() {
        teams
            .teams
            .iter()
            .flat_map(|t| &t.source_repos)
            .filter(matches)
            .collect()
    } else {
        scoped
    };
    if candidates.is_empty() {
        return Ok(false);
    }
    for entry in candidates {
        let candidate = SourceRepoSlug::with_provider_and_host(
            entry.owner.clone(),
            entry.repo.clone(),
            entry.provider_id(),
            entry.host.clone(),
        )
        .map_err(|e| e.to_string())?;
        if candidate.physical_path_key().map_err(|e| e.to_string())? != requested {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn resolve_repository_root(
    org_root: &Path,
    team: &str,
    slug: &SourceRepoSlug,
) -> Result<PathBuf, String> {
    let canonical =
        super::path::try_path_to_team_repo_dir(org_root, team, slug).map_err(|e| e.to_string())?;
    resolve_owned_layout(
        &canonical,
        &LegacyRepositoryPathLocator::team_repo_leaf(org_root, team, slug, ""),
        org_root,
        Some(team),
        slug,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_edits_remain_authoritative_and_preserve_legacy_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("canonical");
        let b = tmp.path().join("legacy");
        for root in [&a, &b] {
            std::fs::create_dir_all(root.join("content")).unwrap();
            std::fs::write(root.join("content/saved.md"), "saved bytes").unwrap();
        }
        assert_eq!(resolve_local_layout(&a, &b).unwrap(), a);
        std::fs::write(a.join("content/saved.md"), "new canonical bytes").unwrap();
        assert_eq!(resolve_local_layout(&a, &b).unwrap(), a);
        std::fs::remove_file(a.join("content/saved.md")).unwrap();
        assert_eq!(resolve_local_layout(&a, &b).unwrap(), a);
        assert_eq!(
            std::fs::read_to_string(b.join("content/saved.md")).unwrap(),
            "saved bytes"
        );
    }

    #[test]
    fn registry_absence_only_defaults_to_exact_cloud_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let cloud = SourceRepoSlug::new("acme", "app").unwrap();
        // Public fields can carry legacy identities outside current constructors.
        let mut enterprise = cloud.clone();
        enterprise.host = Some("github.example.com".into());
        assert!(legacy_owner_is_unique(tmp.path(), Some("default"), &cloud).unwrap());
        assert!(!legacy_owner_is_unique(tmp.path(), Some("default"), &enterprise).unwrap());
        std::fs::write(tmp.path().join("teams.yml"), "teams: []\n").unwrap();
        assert!(!legacy_owner_is_unique(tmp.path(), Some("default"), &cloud).unwrap());
    }

    #[test]
    fn hostless_content_is_not_shared_across_registered_hosts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("teams.yml"), "teams:\n  - name: default\n    source_repos:\n      - owner: acme\n        repo: app\n      - owner: acme\n        repo: app\n        provider: gitlab\n").unwrap();
        let github = SourceRepoSlug::new("acme", "app").unwrap();
        let gitlab =
            SourceRepoSlug::with_provider_and_host("acme", "app", "gitlab", None::<String>)
                .unwrap();
        let legacy =
            LegacyRepositoryPathLocator::team_repo_leaf(root, "default", &github, "content");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("saved.md"), "legacy bytes").unwrap();
        for slug in [&github, &gitlab] {
            assert!(resolve_repository_root(root, "default", slug)
                .unwrap_err()
                .contains("ambiguous across hosts"));
        }
        let canonical =
            super::super::path::try_path_to_team_repo_dir(root, "default", &gitlab).unwrap();
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::write(canonical.join("saved.md"), "gitlab bytes").unwrap();
        assert_eq!(
            resolve_repository_root(root, "default", &gitlab).unwrap(),
            canonical
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("saved.md")).unwrap(),
            "legacy bytes"
        );
    }
}
