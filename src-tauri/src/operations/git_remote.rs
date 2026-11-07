// SPDX-License-Identifier: MIT
//! Git-remote helpers that are independent of the install lifecycle.
//!
//! `derive_slug_from_repo` was previously a private helper in
//! `operations/install.rs`. SS.8 introduced a new caller
//! (`commands::worktree::read_worktree_config`) that needs the same
//! `(owner, repo)` derivation but has no business depending on the
//! install module — which the seamless-sync sub-epic (SS.11) is going to
//! delete wholesale. Relocating here keeps SS.11 a pure-deletion sweep.

use std::path::Path;

use log::debug;

use crate::domain::SourceRepoSlug;

/// Derive a `SourceRepoSlug` (owner/repo) from the source repo's `origin`
/// remote URL.
///
/// Opens the repo with git2, reads the `origin` remote, parses the GitHub
/// URL via the project's normalization rules. Returns `None` if the repo
/// has no `origin` remote or the URL isn't a recognized format — callers
/// treat that as "this repo isn't eligible for slug-keyed lookups" and
/// fall through to whatever fallback they have.
///
/// Log level is `debug!`, not `warn!`: SS.8 made this a hot read-path
/// helper (every settings refresh, every worktree probe) and a repo
/// without an `origin` remote is a legitimate state (local-only clones,
/// fresh `git init`), not a misconfiguration worth warning about
/// repeatedly.
pub fn derive_slug_from_repo(source_repo_path: &Path) -> Option<SourceRepoSlug> {
    let repo = match git2::Repository::open(source_repo_path) {
        Ok(r) => r,
        Err(e) => {
            debug!(target: "git_remote", "cannot open git repo at {}: {}", source_repo_path.display(), e);
            return None;
        }
    };

    let remote = match repo.find_remote("origin") {
        Ok(r) => r,
        Err(e) => {
            debug!(target: "git_remote", "no origin remote in {}: {}", source_repo_path.display(), e);
            return None;
        }
    };

    let url = Result::ok(remote.url())?;
    let normalized = crate::context_repo::normalize_source_repo_url(url).ok()?;

    let host = normalized.normalized_url.split('/').next()?;
    let provider = crate::commands::forge::provider_for_host(host).unwrap_or("git");
    SourceRepoSlug::with_provider_and_host(normalized.owner, normalized.repo, provider, Some(host))
        .ok()
}

/// The identity of a local repository with no remote: `local/<folder>` on the
/// `local` provider/host — the same key a team's `local/<folder>` source repo and a
/// remote-less attachment resolve under. `None` when `folder` is not a valid
/// repo segment.
pub fn local_repo_slug(folder: &str) -> Option<SourceRepoSlug> {
    SourceRepoSlug::with_provider_and_host(
        LOCAL_PROVIDER,
        folder.to_ascii_lowercase(),
        LOCAL_PROVIDER,
        Some(LOCAL_PROVIDER),
    )
    .ok()
}

const LOCAL_PROVIDER: &str = "local";

/// The repository identity for repo-keyed state (worktree config):
/// the `origin` slug when there is one, else — for a git repo with **no** `origin`
/// remote — its [`local_repo_slug`], named after the main checkout's folder so a
/// linked worktree resolves to the same identity as its source repo.
///
/// An `origin` that exists but isn't a recognized forge URL stays `None`: the repo
/// claims a remote identity we can't read, so we don't guess a local one.
pub fn derive_repository_identity(repo_path: &Path) -> Option<SourceRepoSlug> {
    let repo = git2::Repository::open(repo_path).ok()?;
    if repo.find_remote("origin").is_ok() {
        return derive_slug_from_repo(repo_path);
    }
    let main_checkout = if repo.is_worktree() {
        // A linked worktree's gitdir names the shared git dir in `commondir`: the
        // main checkout's `.git`, or the repo itself when it is bare.
        let gitdir = repo.path();
        let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
        let common = gitdir.join(common.trim()).canonicalize().ok()?;
        if common.file_name()? == ".git" {
            common.parent()?.to_path_buf()
        } else {
            common
        }
    } else {
        repo.workdir()?.canonicalize().ok()?
    };
    let folder = main_checkout.file_name()?.to_str()?;
    local_repo_slug(folder.strip_suffix(".git").unwrap_or(folder))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_provider_from_a_non_github_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(tmp.path()).unwrap();
        repository
            .remote("origin", "git@gitlab.com:acme/widget.git")
            .unwrap();

        let slug = derive_slug_from_repo(tmp.path()).unwrap();
        assert_eq!(slug.owner, "acme");
        assert_eq!(slug.repo, "widget");
        assert_eq!(slug.provider, "gitlab");
    }

    fn committed_repo(path: &Path) -> git2::Repository {
        let repo = git2::Repository::init(path).unwrap();
        std::fs::write(path.join("a.txt"), "a").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        {
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = git2::Signature::now("t", "t@example.com").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        repo
    }

    #[test]
    fn a_repo_without_origin_is_identified_as_a_local_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("Sample-Api");
        let repo = committed_repo(&source);
        let linked = tmp.path().join("elsewhere");
        repo.worktree("feat", &linked, None).unwrap();

        let expected = local_repo_slug("sample-api").unwrap();
        assert_eq!(expected.remote_key(), "local/local/sample-api");
        assert_eq!(derive_repository_identity(&source), Some(expected.clone()));
        assert_eq!(
            derive_repository_identity(&linked),
            Some(expected),
            "a linked worktree shares its source repo's identity"
        );
        assert_eq!(derive_slug_from_repo(&source), None);
    }

    #[test]
    fn a_worktree_of_a_bare_repo_is_named_after_the_bare_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("seed");
        committed_repo(&source);
        let bare = git2::build::RepoBuilder::new()
            .bare(true)
            .clone(source.to_str().unwrap(), &tmp.path().join("Tool.git"))
            .unwrap();
        bare.remote_delete("origin").unwrap();
        let linked = tmp.path().join("wt");
        bare.worktree("feat", &linked, None).unwrap();

        assert_eq!(derive_repository_identity(&linked), local_repo_slug("tool"),);
    }

    #[test]
    fn repository_identity_prefers_origin_and_never_guesses() {
        let tmp = tempfile::tempdir().unwrap();
        let forge = tmp.path().join("forge");
        git2::Repository::init(&forge)
            .unwrap()
            .remote("origin", "https://github.com/acme/api.git")
            .unwrap();
        assert_eq!(
            derive_repository_identity(&forge),
            derive_slug_from_repo(&forge)
        );
        assert_eq!(derive_repository_identity(&forge).unwrap().owner, "acme");

        let odd = tmp.path().join("odd");
        git2::Repository::init(&odd)
            .unwrap()
            .remote("origin", "not a url")
            .unwrap();
        assert_eq!(derive_repository_identity(&odd), None);

        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(derive_repository_identity(&plain), None);
    }
}
