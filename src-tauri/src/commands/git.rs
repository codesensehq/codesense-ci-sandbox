// SPDX-License-Identifier: MIT
use super::system::contexts_dir;

/// Sanitize a repo path into a safe directory name.
fn sanitize_repo_name(repo_path: &str) -> Result<String, String> {
    let name = std::path::Path::new(repo_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    super::paths::sanitize_name(&name)
        .map_err(|_| format!("invalid repo path: cannot derive name from '{}'", repo_path))
}

#[tauri::command]
pub async fn get_context_repo_path(
    app: tauri::AppHandle,
    repo_path: String,
) -> Result<String, String> {
    let repo_name = sanitize_repo_name(&repo_path)?;
    let ctx_dir = contexts_dir(&app)?.join(&repo_name);

    crate::commands::await_blocking("get_context_repo_path", move || {
        if ctx_dir.exists() {
            Ok(ctx_dir.display().to_string())
        } else {
            Err(format!("context repo not found for {}", repo_name))
        }
    })
    .await
}

#[tauri::command]
pub async fn check_context_repo_remote(
    app: tauri::AppHandle,
    repo_path: String,
) -> Result<Option<String>, String> {
    let repo_name = sanitize_repo_name(&repo_path)?;
    let ctx_dir = contexts_dir(&app)?.join(&repo_name);

    crate::commands::await_blocking("check_context_repo_remote", move || {
        if !ctx_dir.exists() {
            return Ok(None);
        }

        let repo = git2::Repository::open(&ctx_dir)
            .map_err(|e| format!("failed to open context repo: {}", e))?;

        let url = repo
            .find_remote("origin")
            .ok()
            .and_then(|r| Result::ok(r.url()).map(|u| u.to_string()));
        Ok(url)
    })
    .await
}

#[tauri::command]
pub async fn reveal_context_repo(app: tauri::AppHandle, repo_path: String) -> Result<(), String> {
    let repo_name = sanitize_repo_name(&repo_path)?;
    let ctx_dir = contexts_dir(&app)?.join(&repo_name);

    crate::commands::await_blocking("reveal_context_repo", move || {
        if !ctx_dir.exists() {
            return Err(format!("context repo not found for {}", repo_name));
        }

        // Open in Finder via macOS `open` command
        std::process::Command::new("open")
            .arg(&ctx_dir)
            .spawn()
            .map_err(|e| format!("failed to open Finder: {}", e))?;

        Ok(())
    })
    .await
}

/// Open an arbitrary directory in Finder. Used by Settings to reveal
/// CodeSense-managed dirs (team content and the like) so users can drop
/// files in or edit them externally.
///
/// Safety: rejects paths outside the codesense-managed roots
/// (`{codesense_home}/users/` and `{codesense_home}/core/`). Same
/// allow-list used by `validate_context_path` for read-side IPC —
/// keeps reveal in line with the rest of the file-IPC surface.
#[tauri::command]
pub async fn reveal_path(path: String) -> Result<(), String> {
    crate::commands::await_blocking("reveal_path", move || {
        let target = std::path::PathBuf::from(&path);

        // Normalize and verify it lives under a codesense-managed root.
        let mut normalized = std::path::PathBuf::new();
        for component in target.components() {
            match component {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                std::path::Component::CurDir => {}
                other => normalized.push(other),
            }
        }
        let home = crate::commands::paths::codesense_home_dir()
            .ok_or_else(|| "codesense home unavailable".to_string())?;
        let users_root = home.join(crate::context_repo::USERS_DIR);
        let core_root = home.join("core");
        let allowed = normalized.starts_with(&users_root) || normalized.starts_with(&core_root);
        if !allowed {
            return Err(format!(
                "refusing to reveal path outside codesense home: {}",
                path
            ));
        }

        if !target.exists() {
            return Err(format!("path does not exist: {}", path));
        }

        std::process::Command::new("open")
            .arg(&target)
            .spawn()
            .map_err(|e| format!("failed to open Finder: {}", e))?;

        Ok(())
    })
    .await
}

/// Return the URL of the `origin` remote for a source repo on disk.
/// - Repo with origin → `Ok(Some(url))`
/// - Repo without origin → `Ok(None)`
/// - Not a git repo → `Err`
#[tauri::command]
pub async fn read_source_repo_origin_url(repo_path: String) -> Result<Option<String>, String> {
    crate::commands::await_blocking("read_source_repo_origin_url", move || {
        origin_url_at(std::path::Path::new(&repo_path))
    })
    .await
}

/// The synchronous read behind [`read_source_repo_origin_url`].
pub(crate) fn origin_url_at(repo_path: &std::path::Path) -> Result<Option<String>, String> {
    let repo =
        git2::Repository::open(repo_path).map_err(|e| format!("not a git repository: {e}"))?;
    Ok(repo
        .find_remote("origin")
        .ok()
        .and_then(|r| Result::ok(r.url()).map(|u| u.to_string())))
}

/// Every remote of the repo at `repo_path` as `(name, url)`, in the order git
/// lists them. A remote without a UTF-8 URL is skipped.
pub(crate) fn remotes_at(repo_path: &std::path::Path) -> Result<Vec<(String, String)>, String> {
    let repo =
        git2::Repository::open(repo_path).map_err(|e| format!("not a git repository: {e}"))?;
    let names = repo.remotes().map_err(|e| e.to_string())?;
    Ok(names
        .iter()
        .filter_map(|name| Option::flatten(Result::ok(name)))
        .filter_map(|name| {
            let url = Result::ok(repo.find_remote(name).ok()?.url())?.to_string();
            Some((name.to_string(), url))
        })
        .collect())
}

/// The working-tree root of the repository containing `path` (searching
/// upward, as git does), canonicalized; `None` when `path` is in no
/// non-bare repository.
pub(crate) fn working_tree_root_of(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let repo = git2::Repository::discover(path).ok()?;
    repo.workdir()?.canonicalize().ok()
}

/// Whether `path` itself is a live git working tree. Attaching a clone
/// requires one, so codesense never tries to write `.git/info/exclude`
/// against a directory that has no `.git` to hold it.
///
/// Strictness contract:
/// - **No upward search.** Uses `RepositoryOpenFlags::NO_SEARCH` so picking
///   `~/Downloads/random-dir` inside a dotfiles-tracked `$HOME` returns
///   `false` instead of resolving to the ancestor repo and silently
///   polluting it. Without this flag, `git2::Repository::open` walks up
///   parent directories looking for `.git` — wrong answer for our use.
/// - **Bare repos rejected.** A bare repo opens successfully but has no
///   working tree to materialize into; we return `false` so the user
///   picks a real clone.
/// - **Picked path must BE the working tree, not a subpath of a `.git`
///   directory.** Iter-2 fix: a user with "Show Hidden Files" can pick
///   `~/myrepo/.git`. `Repository::open_ext` succeeds in that case, but
///   `repo.workdir()` resolves to the parent (`~/myrepo`), not the
///   picked path. Reject when `workdir() != Some(canonicalized(path))`.
/// - **Linked worktrees accepted.** The worktree's `.git` is a gitdir
///   pointer file rather than a directory; `open_ext` still resolves it
///   without walking up (the pointer is treated as the repo discovery
///   surface for the picked dir specifically), and `workdir()` matches
///   the picked path.
/// - **Submodules accepted.** A submodule's `.git` is a `gitlink` file
///   into the superproject's `.git/modules/<name>/`. CodeSense's
///   downstream `git_exclude::add_entries` will write into that modules
///   path; git honors the exclusion from inside the submodule. Documented
///   as accepted behavior — picking a submodule means accepting that
///   the exclude file lives in the superproject's git dir. Pinned by
///   `is_git_working_tree_true_for_submodule`.
///
/// All open failures collapse to `false`. We deliberately do not
/// distinguish "not a git repository" from rare path-level I/O failures
/// (permission denied, broken mount): a path the OS can't probe isn't
/// usable for attach either. Synchronous git I/O: async callers run it on
/// the blocking pool.
pub(crate) fn is_git_working_tree(path: &std::path::Path) -> bool {
    match git2::Repository::open_ext(
        path,
        git2::RepositoryOpenFlags::NO_SEARCH,
        &[] as &[&std::path::Path],
    ) {
        Ok(repo) if !repo.is_bare() => {
            // Reject a picked path that is not the working tree itself (e.g.
            // `~/repo/.git`). Canonicalize both sides so symlinks and trailing
            // slashes don't produce false negatives.
            let picked = path.canonicalize().ok();
            let workdir = repo.workdir().and_then(|w| w.canonicalize().ok());
            matches!((picked, workdir), (Some(p), Some(w)) if p == w)
        }
        _ => false,
    }
}

/// A tab's resolved source repo (if any). The per-tab primitives toolbar
/// uses this one call to decide whether to light the Source + Editor icons
/// on a plain CLI tab; agent tiles whose cwd is a worktree by construction
/// use the same call for symmetry.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabSourceRepo {
    /// Canonical path to the working-tree root (via `git_discover`).
    pub root: String,
    /// `true` iff `cwd` itself is the working-tree root (we only light the
    /// per-tab toolbar when the cwd IS the root — a cd into a subdir still
    /// resolves the same root but needs the live-tab-cwd follow-up to be
    /// safe for the editor, which keys the tree off `ws.cwd`).
    pub is_root: bool,
}

/// Resolve the source repo for a tab's cwd. Returns `None` for a cwd that is
/// not inside any git working tree (or whose only enclosing repo is bare).
/// Synchronous git I/O: callers run it on the blocking pool via `spawn_blocking`.
#[tauri::command]
pub async fn resolve_tab_source_repo(cwd: String) -> Option<TabSourceRepo> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = std::path::Path::new(&cwd);
        if !path.is_dir() {
            return None;
        }
        let root = working_tree_root_of(path)?;
        let canonical_cwd = path.canonicalize().ok()?;
        Some(TabSourceRepo {
            is_root: canonical_cwd == root,
            root: root.display().to_string(),
        })
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_repo_name() {
        assert_eq!(
            sanitize_repo_name("/Users/dev/mycorp_app").unwrap(),
            "mycorp-app"
        );
        assert_eq!(
            sanitize_repo_name("/Users/dev/Document Integrations").unwrap(),
            "document-integrations"
        );
        assert_eq!(
            sanitize_repo_name("/Users/dev/My Project!@#").unwrap(),
            "my-project"
        );
        assert_eq!(sanitize_repo_name("/Users/dev/simple").unwrap(), "simple");
        assert!(sanitize_repo_name("/").is_err());
        assert!(sanitize_repo_name("").is_err());
    }

    #[test]
    fn test_context_repo_creation_with_git2() {
        let dir = std::env::temp_dir()
            .join("codesense_test")
            .join(format!("git2_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Init a repo with git2
        let repo = git2::Repository::init(&dir).unwrap();
        assert!(dir.join(".git").exists());

        // Verify no remote
        assert!(repo.find_remote("origin").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- read_source_repo_origin_url ---

    #[tokio::test]
    async fn read_origin_url_repo_with_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        repo.remote("origin", "https://github.com/org/repo.git")
            .unwrap();

        let result = read_source_repo_origin_url(tmp.path().to_str().unwrap().into()).await;
        assert_eq!(result, Ok(Some("https://github.com/org/repo.git".into())));
    }

    #[tokio::test]
    async fn read_origin_url_repo_without_origin() {
        let tmp = tempfile::tempdir().unwrap();
        git2::Repository::init(tmp.path()).unwrap();

        let result = read_source_repo_origin_url(tmp.path().to_str().unwrap().into()).await;
        assert_eq!(result, Ok(None));
    }

    #[tokio::test]
    async fn read_origin_url_not_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let result = read_source_repo_origin_url(tmp.path().to_str().unwrap().into()).await;
        assert!(result.is_err());
    }

    // --- is_git_working_tree (SS.5 AC-5) ---

    #[test]
    fn is_git_working_tree_true_for_initialized_repo() {
        let tmp = tempfile::tempdir().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        let result = is_git_working_tree(tmp.path());
        assert!(result);
    }

    #[test]
    fn is_git_working_tree_false_for_plain_directory() {
        let tmp = tempfile::tempdir().unwrap();
        // No git init — plain empty dir.
        let result = is_git_working_tree(tmp.path());
        assert!(!result);
    }

    #[test]
    fn is_git_working_tree_false_for_nonexistent_path() {
        // git2 surfaces this as an Os/NotFound error; we collapse to false
        // because the attach dialog can't use it either way.
        let result =
            is_git_working_tree(std::path::Path::new("/nonexistent/path/that/cannot/exist"));
        assert!(!result);
    }

    #[test]
    fn is_git_working_tree_false_for_subdir_of_ancestor_repo() {
        // PAT-1 / PAT-2: NO_SEARCH must stop the upward walk. Previously
        // `Repository::open` resolved any non-git subdirectory of any
        // ancestor git repo as `true`, silently routing exclusion writes
        // into the ancestor's `.git/info/exclude` — a real risk on any
        // dotfiles-tracked $HOME or other ambient git working tree.
        let tmp = tempfile::tempdir().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        let subdir = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&subdir).unwrap();
        let result = is_git_working_tree(std::path::Path::new(subdir.to_str().unwrap()));
        assert!(!result);
    }

    #[test]
    fn is_git_working_tree_false_for_bare_repo() {
        // PAT-3: a bare repo opens successfully but has no working tree
        // for codesense to materialize into. Reject.
        let tmp = tempfile::tempdir().unwrap();
        git2::Repository::init_bare(tmp.path()).unwrap();
        let result = is_git_working_tree(tmp.path());
        assert!(!result);
    }

    #[test]
    fn is_git_working_tree_false_when_picking_dot_git_dir_itself() {
        // Iter-2 E4: a user with "Show Hidden Files" can select
        // `~/myrepo/.git`. `Repository::open_ext(NO_SEARCH)` succeeds (the
        // gitdir IS a valid repo location), but `repo.workdir()` resolves to
        // the parent. Reject so codesense doesn't attach a path whose
        // workdir mismatches the user's intent.
        let tmp = tempfile::tempdir().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        let git_dir = tmp.path().join(".git");
        assert!(git_dir.is_dir(), "expected a real .git directory");
        let result = is_git_working_tree(std::path::Path::new(git_dir.to_str().unwrap()));
        assert!(!result);
    }

    #[test]
    fn is_git_working_tree_true_for_submodule() {
        // Iter-2 E3: pins the doc claim that submodules are accepted. A
        // submodule's `.git` is a `gitlink` file pointing into the
        // superproject's `.git/modules/<name>/`. The probe must return
        // `true`, and downstream consumers (git_exclude::add_entries) write
        // into the superproject's modules dir — see
        // `commands/git.rs::is_git_working_tree` doc comment.
        let super_root = tempfile::tempdir().unwrap();
        let super_repo = git2::Repository::init(super_root.path()).unwrap();

        // Seed super so it has HEAD (required before submodule add).
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let tree_oid = {
            let mut idx = super_repo.index().unwrap();
            idx.write_tree().unwrap()
        };
        let tree = super_repo.find_tree(tree_oid).unwrap();
        super_repo
            .commit(Some("HEAD"), &sig, &sig, "seed-super", &tree, &[])
            .unwrap();

        // Create a local "remote" repo for the submodule to point at.
        let sub_remote = tempfile::tempdir().unwrap();
        let sub_remote_repo = git2::Repository::init(sub_remote.path()).unwrap();
        let sub_tree_oid = {
            let mut idx = sub_remote_repo.index().unwrap();
            idx.write_tree().unwrap()
        };
        let sub_tree = sub_remote_repo.find_tree(sub_tree_oid).unwrap();
        sub_remote_repo
            .commit(Some("HEAD"), &sig, &sig, "seed-sub", &sub_tree, &[])
            .unwrap();

        // Add a submodule via subprocess (git2's Submodule API requires
        // multiple steps + manual fetch; the CLI is simpler and matches
        // what real users do).
        let sub_url = format!("file://{}", sub_remote.path().display());
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(super_root.path())
            .args([
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                &sub_url,
                "inner",
            ])
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .expect("git submodule add");
        if !status.success() {
            // Some CI environments may forbid file:// submodules entirely.
            // The doc claim is still correct; skip rather than fail-flaky.
            eprintln!("submodule add did not succeed; skipping submodule probe test");
            return;
        }

        let sub_path = super_root.path().join("inner");
        let sub_git_meta = std::fs::symlink_metadata(sub_path.join(".git")).unwrap();
        assert!(
            sub_git_meta.file_type().is_file(),
            "expected submodule .git to be a gitlink file"
        );

        let result = is_git_working_tree(std::path::Path::new(sub_path.to_str().unwrap()));
        assert!(result);
    }

    #[test]
    fn is_git_working_tree_true_inside_linked_worktree() {
        // A linked worktree counts as a working tree — Repository::open
        // resolves the `.git` gitdir pointer file the same way it does a
        // `.git` directory.
        let main = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(main.path()).unwrap();
        // Need at least one commit before `git worktree add`.
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let tree_oid = {
            let mut idx = repo.index().unwrap();
            idx.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_oid).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "seed", &tree, &[])
            .unwrap();

        let wt_root = tempfile::tempdir().unwrap();
        let wt_path = wt_root.path().join("wt1");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(main.path())
            .args(["worktree", "add", "--quiet", "-B", "ss5-wt"])
            .arg(&wt_path)
            .status()
            .expect("git worktree add");
        assert!(status.success());

        let result = is_git_working_tree(std::path::Path::new(wt_path.to_str().unwrap()));
        assert!(result);
    }
}
