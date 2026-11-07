// SPDX-License-Identifier: MIT
//! Shared enumeration of every org-root directory under
//! `{codesense_home}/users/*/orgs/`.
//!
//! Pure filesystem walk — the shared primitive for whole-install boot sweeps
//! (e.g. SLW-IN-6's orphan prune). Knows nothing about teams, the inheritance
//! model, or the (removed) org-root team.

use std::path::{Path, PathBuf};

use crate::domain::RegistryFile;

use super::path::{ORGS_DIR, USERS_DIR};

/// Every local principal with stored data, including profiles without a numeric identity.
pub(crate) fn principals(home: &Path) -> Vec<crate::domain::Principal> {
    std::result::Result::ok(std::fs::read_dir(home.join("users")))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            std::result::Result::ok(crate::domain::Principal::parse_local(
                &std::fs::DirEntry::file_name(&entry).to_string_lossy(),
            ))
        })
        .collect()
}

/// Enumerate every org-root directory under `{codesense_home}/users/*/orgs/`.
/// Shared by the boot sweeps that touch all context repos. A missing tree
/// (fresh install) yields an empty list — never an error; any other read error
/// is logged so a silently-empty sweep is diagnosable.
pub(crate) fn enumerate_all_org_roots(codesense_home: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let users_dir = codesense_home.join(USERS_DIR);
    let user_entries = match std::fs::read_dir(&users_dir) {
        Ok(rd) => rd,
        // A missing users/ tree (fresh install) is the normal empty case — silent.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return roots,
        // Any other error (e.g. permissions) is abnormal: log so a silently-empty
        // sweep is diagnosable, rather than swallowing it as "no orgs".
        Err(e) => {
            log::warn!(
                target: "context_repo::org_enumeration",
                "could not read {}: {e} — treating as no orgs this boot",
                users_dir.display()
            );
            return roots;
        }
    };
    for user in user_entries.flatten() {
        let orgs_dir = user.path().join(ORGS_DIR);
        if !orgs_dir.is_dir() {
            continue;
        }
        roots.extend(enumerate_org_roots(&orgs_dir));
    }
    roots
}

/// `(org_slug, org_root)` for every org under a single user's `orgs/` dir. Pairs
/// each recognized root with the slug parsed from its path, so callers that need
/// the slug — the install-state rebuild-from-JSON
/// (`crate::content_engine::sync_state_store::import_all_from_json`) — do not
/// re-derive it. Same `org.yml`-marker recognition as [`enumerate_all_org_roots`],
/// scoped to one user. A missing tree yields an empty list.
pub(crate) fn enumerate_user_orgs(codesense_home: &Path, user_id: u64) -> Vec<(String, PathBuf)> {
    let orgs_dir = codesense_home
        .join(USERS_DIR)
        .join(user_id.to_string())
        .join(ORGS_DIR);
    enumerate_org_roots(&orgs_dir)
        .into_iter()
        .filter_map(|root| {
            let slug = super::path::org_slug_from_org_root(&root.to_string_lossy())?.to_owned();
            Some((slug, root))
        })
        .collect()
}

/// Enumerate org-root directories under `orgs/`. An org slug is either a single
/// segment (`orgs/{slug}`) or nested `owner/repo` (`orgs/{owner}/{repo}`). A
/// directory is an org root iff it directly contains an `org.yml` file;
/// otherwise we descend exactly one level to find nested roots.
fn enumerate_org_roots(orgs_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let Ok(entries) = std::fs::read_dir(orgs_dir) else {
        return roots;
    };
    let marker = RegistryFile::OrgYml.filename();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.join(marker).is_file() {
            roots.push(path);
        } else if let Ok(children) = std::fs::read_dir(&path) {
            for child in children.flatten() {
                let child_path = child.path();
                if child_path.is_dir() && child_path.join(marker).is_file() {
                    roots.push(child_path);
                }
            }
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn empty_when_home_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(enumerate_all_org_roots(&tmp.path().join("nope")).is_empty());
    }

    #[test]
    fn empty_when_no_users_tree() {
        let tmp = TempDir::new().unwrap();
        assert!(enumerate_all_org_roots(tmp.path()).is_empty());
    }

    #[test]
    fn finds_single_segment_org_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp
            .path()
            .join(USERS_DIR)
            .join("42")
            .join(ORGS_DIR)
            .join("acme");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(RegistryFile::OrgYml.filename()), "").unwrap();
        assert_eq!(enumerate_all_org_roots(tmp.path()), vec![root]);
    }

    #[test]
    fn finds_nested_owner_repo_org_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp
            .path()
            .join(USERS_DIR)
            .join("42")
            .join(ORGS_DIR)
            .join("acme")
            .join("api");
        // The `org.yml` marker lives one level down (owner/repo layout).
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(RegistryFile::OrgYml.filename()), "").unwrap();
        assert_eq!(enumerate_all_org_roots(tmp.path()), vec![root]);
    }
}
