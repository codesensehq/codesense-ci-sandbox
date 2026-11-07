// SPDX-License-Identifier: MIT
use std::path::PathBuf;

use crate::domain::{load_org, CredentialRef, LoadedOrg, Principal};

fn read_org_binding_best_effort(path: &std::path::Path, slug: &str) -> Option<CredentialRef> {
    crate::context_repo::org_bindings::read_org_bindings(path)
        .ok()?
        .bindings
        .get(&crate::context_repo::org_bindings::normalize_org_slug(slug))
        .cloned()
}

// Display-only "which forge?" badge. Accepts anything that lets us read a
// plausible host — a port-form SSH URL (`ssh://git@github.com:22/o/r.git`)
// still resolves to `github` here where the retired context-repo normalizer
// would have refused it as `UnexpectedPathSegments`; the badge is cosmetic,
// so the extra admit is fine. `provider_for_host` is case-insensitive on the
// host match, so we only need the leading `www.` strip to be case-insensitive
// too (`https://WWW.github.com/...` must still read as `github`).
fn context_repo_provider(root: &std::path::Path) -> Option<String> {
    let repository = git2::Repository::open(root).ok()?;
    let remote = repository.find_remote("origin").ok()?;
    let url = Result::ok(remote.url())?.trim();
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let after_userinfo = after_scheme
        .split_once('@')
        .map_or(after_scheme, |(_, rest)| rest);
    let host_end = after_userinfo
        .find(['/', ':'])
        .unwrap_or(after_userinfo.len());
    let host = &after_userinfo[..host_end];
    // SW-CR-5: `is_char_boundary(4)` guards the byte slice on a host whose
    // byte 4 lands mid-codepoint (e.g. `飯飯.com` — 3-byte chars). git2 only
    // surfaces ASCII hosts in practice, but the hardened guard avoids any
    // future UTF-8 panic path. The `host.len() > 4` precondition is kept so
    // a degenerate `"www."` alone stays untouched (would otherwise strip to
    // `""` since `is_char_boundary(len) == true`).
    let host =
        if host.len() > 4 && host.is_char_boundary(4) && host[..4].eq_ignore_ascii_case("www.") {
            &host[4..]
        } else {
            host
        };
    crate::commands::forge::provider_for_host(host).map(str::to_string)
}

/// The registry of a just-attached (or just-created) checkout, carrying its
/// origin and forge from the local `.git/config`. Never touches the network.
pub(crate) fn load_attached_org(slug: &str, root: &std::path::Path) -> LoadedOrg {
    let mut loaded = load_org(slug, root, crate::commands::org::origin_of(root).1);
    if let Some(org) = loaded.org.as_mut() {
        org.remote_provider = context_repo_provider(root);
    }
    loaded
}

/// Front-door command the frontend calls to load an org's registry.
///
/// Reads the working tree for every org (SW-ID-9); `remote_url` is passed
/// through to `load_org` for the org's remote identity only and does not
/// select the read source. Runs on the blocking pool via `await_blocking`,
/// so the (now `std::fs::read`-only) load never touches the UI thread.
#[tauri::command]
pub async fn load_org_cmd(
    _app: tauri::AppHandle,
    slug: String,
    org_root: String,
    remote_url: Option<String>,
    principal: Option<Principal>,
) -> Result<LoadedOrg, String> {
    let loaded = crate::commands::await_blocking("load_org_cmd", move || {
        let slug = slug.trim().to_string();
        if slug.is_empty() {
            return Err("slug must not be empty".to_string());
        }

        let root = PathBuf::from(&org_root);
        if !root.is_absolute() {
            return Err(format!("org_root must be absolute: {}", org_root));
        }

        let meta = std::fs::metadata(&root)
            .map_err(|err| format!("cannot access org_root '{}': {}", org_root, err))?;
        if !meta.is_dir() {
            return Err(format!("org_root is not a directory: {}", org_root));
        }

        let remote_provider = context_repo_provider(&root);

        // SW-CR-8: fold any legacy `{team}/install/` tree into `{team}/content/`
        // before composing the registry — on an un-folded repo the install tree
        // is invisible to the resolver, launching Claude agents against it
        // aborts on the notify hook, and the user has no legible diagnostic
        // path. Idempotent (short-circuits on `.folded`); on-disk-only (no
        // auto-commit); conflicts are left in place for `check_install_leftovers`
        // to surface and the heal command to re-run.
        match crate::context_repo::migrate_install_tree::fold_org_install_tree(&root, false) {
            Ok(report) if !report.is_noop() => {
                log::info!(
                    target: "registry",
                    "load_org_cmd {slug}: folded install/ → content/ (moved={}, conflicts={}, teams={})",
                    report.total_moved(),
                    report.total_conflicts(),
                    report.per_team.len(),
                );
            }
            Ok(_) => {}
            Err(err) => {
                log::warn!(
                    target: "registry",
                    "load_org_cmd {slug}: fold_org_install_tree failed: {err}",
                );
            }
        }

        let mut loaded = load_org(slug.clone(), &root, remote_url);
        if let Some(org) = loaded.org.as_mut() {
            org.remote_provider = remote_provider;
            org.resolved_repository_paths = org
                .teams
                .iter()
                .flat_map(|team| {
                    team.source_repos.iter().map(|repo| {
                        let key = format!("{}\n{}", team.name, repo.remote_key());
                        let path = crate::context_repo::layout_resolver::resolve_repository_root(
                            &root, &team.name, repo,
                        )?;
                        Ok((key, path.to_string_lossy().into_owned()))
                    })
                })
                .collect::<Result<_, String>>()?;
        }
        if let (Some(principal), Some(org)) = (principal.as_ref(), loaded.org.as_mut()) {
            let home = crate::commands::paths::codesense_home_dir()
                .ok_or_else(|| "cannot resolve home directory".to_string())?;
            if crate::context_repo::context_attachments::registered(&home, principal, &slug)?
                .is_none()
            {
                let path = crate::context_repo::path_to_user_org_bindings_file(&home, principal);
                org.credential_ref = read_org_binding_best_effort(&path, &slug);
            } else {
                let expected = crate::context_repo::path_to_org_root(&home, principal, &slug)?;
                if root.canonicalize().map_err(|e| e.to_string())? != expected {
                    return Err("Attached context root does not match its registration".into());
                }
            }
        }
        Ok(loaded)
    })
    .await?;

    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_binding_sidecar_is_ignored_and_left_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("org_bindings.yml");
        let corrupt = b"bindings: [[[";
        std::fs::write(&path, corrupt).unwrap();

        assert_eq!(read_org_binding_best_effort(&path, "owner/repo"), None);
        assert_eq!(std::fs::read(path).unwrap(), corrupt);
    }

    #[test]
    fn context_provider_reads_a_forge_from_each_common_remote_url_form() {
        // Covers the common remote-URL spellings a context repo origin can take.
        // Behavior must match the retired `normalize_context_repo_url` path on
        // every row — the badge is display-only and the story's non-goal is
        // explicit: do not change `context_repo_provider`'s outputs.
        let cases: &[(&str, Option<&str>)] = &[
            ("git@gitlab.com:acme/context.git", Some("gitlab")),
            ("https://github.com/acme/context.git", Some("github")),
            ("https://www.github.com/acme/context", Some("github")),
            // Case-insensitive `www.` strip — R2-M1 regression guard.
            ("https://WWW.github.com/acme/context", Some("github")),
            ("ssh://git@github.com/acme/context.git", Some("github")),
            ("https://bitbucket.org/acme/context", Some("bitbucket")),
            // Unknown host → no provider.
            ("https://gitea.example.com/acme/context", None),
        ];
        for (remote, expected) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let repository = git2::Repository::init(tmp.path()).unwrap();
            repository.remote("origin", remote).unwrap();
            assert_eq!(
                context_repo_provider(tmp.path()).as_deref(),
                *expected,
                "remote={remote:?}",
            );
        }
    }

    #[test]
    fn context_provider_is_none_without_an_origin_remote() {
        let tmp = tempfile::tempdir().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        assert_eq!(context_repo_provider(tmp.path()), None);
    }

    #[test]
    fn load_attached_org_carries_origin_and_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let root = crate::context_repo::context_attachments::tests::checkout(
            tmp.path(),
            "checkout",
            "acme/context",
        );
        let unpublished = load_attached_org("acme/context", &root).org.unwrap();
        assert_eq!(unpublished.remote_url, None);
        assert_eq!(unpublished.remote_provider, None);

        git2::Repository::open(&root)
            .unwrap()
            .remote("origin", "https://bob:ghp_x@github.com/o/r.git") // trufflehog:ignore
            .unwrap();
        let org = load_attached_org("acme/context", &root).org.unwrap();
        assert_eq!(
            org.remote_url.as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert_eq!(org.remote_provider.as_deref(), Some("github"));
    }
}
