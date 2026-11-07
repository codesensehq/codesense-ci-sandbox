// SPDX-License-Identifier: MIT
use std::path::Path;

use serde::Serialize;

use crate::context_repo::{path_to_user_home, ORGS_DIR};
use crate::domain::{descriptor_exhausted, retry_descriptor_io, LoadedOrg, Principal, RawOrgYml};

use super::async_helpers::await_blocking;
use super::paths::codesense_home_dir;

// ---------- Types ----------

#[derive(Debug, Clone, Serialize)]
pub struct OrgSummary {
    pub is_attached: bool,
    pub availability_error: Option<String>,
    pub slug: String,
    pub display_name: String,
    pub has_remote: bool,
    /// Origin URL from the context repo's `.git/config`. `Some` when the
    /// repo has an `origin` remote whose URL is valid UTF-8; `None`
    /// otherwise. Note: `has_remote` can be true while `remote_url` is
    /// `None` (non-UTF-8 URLs — theoretical for our user base, but the
    /// boundary is still typed for it — or a URL that can't be shown without
    /// risking a credential; see [`redact_origin_url`]). Credentials, query
    /// and fragment are removed. Attached checkouts report their origin too;
    /// CodeSense still never fetches, pulls or pushes them. Source of truth
    /// is git, not `org.yml`. See F-38.
    pub remote_url: Option<String>,
    pub org_root: String,
}

#[cfg(test)]
mod connection_binding_command_tests {}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectOrgResult {
    pub org_slug: String,
    pub org_root: String,
    // "cloned", "already_existed" or "adopted" (seeded from the user's own
    // clone, no network) — a JSON-friendly tagged string so the frontend
    // doesn't have to special-case an enum round-trip.
    pub clone_outcome: String,
    pub loaded_org: LoadedOrg,
}

// ---------- Commands ----------

#[tauri::command]
pub async fn discover_local_orgs(github_user_id: u64) -> Result<Vec<OrgSummary>, String> {
    await_blocking("discover_local_orgs", move || {
        let home = codesense_home_dir().ok_or("cannot resolve home directory")?;
        let principal = Principal::from(github_user_id);
        discover_local_orgs_impl(&home, &principal)
    })
    .await
}

// `pub(crate)` so SW-AU-24's `enumerate_orgs_teams` reuses the same clone-scan
// (nested + legacy layout, symlink guard, slug validation) instead of walking
// the orgs dir a second time.
pub(crate) fn discover_local_orgs_impl(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<Vec<OrgSummary>, String> {
    let attachments = crate::context_repo::context_attachments::list(codesense_home, principal)?;
    let mut summaries = discover_managed_orgs_impl(codesense_home, principal)?;
    for attachment in attachments {
        if summaries.iter().any(|s| s.slug == attachment.slug) {
            return Err(format!("Context {} exists in both managed storage and the attachment registry; detach the conflicting registration", attachment.slug));
        }
        let availability_error =
            crate::context_repo::context_attachments::available_root(&attachment).err();
        summaries.push(attached_summary(attachment, availability_error));
    }
    summaries.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(summaries)
}

/// Summary row for an attached checkout. Reads the origin only when the root is
/// available, so a missing checkout is never opened.
pub(crate) fn attached_summary(
    attachment: crate::context_repo::context_attachments::ContextAttachment,
    availability_error: Option<String>,
) -> OrgSummary {
    let (has_remote, remote_url) = if availability_error.is_none() {
        origin_of(&attachment.root)
    } else {
        (false, None)
    };
    OrgSummary {
        slug: attachment.slug,
        display_name: attachment.display_name,
        org_root: attachment.root.to_string_lossy().into_owned(),
        is_attached: true,
        availability_error,
        has_remote,
        remote_url,
    }
}

/// The checkout's `origin`: whether it exists, and its URL with credentials
/// removed (see [`redact_origin_url`]). Local `.git/config` read; never touches
/// the network.
pub(crate) fn origin_of(path: &Path) -> (bool, Option<String>) {
    match git2::Repository::open(path) {
        Ok(repo) => match repo.find_remote("origin") {
            Ok(remote) => (true, Result::ok(remote.url()).and_then(redact_origin_url)),
            Err(_) => (false, None),
        },
        Err(_) => (false, None),
    }
}

/// An origin URL safe to show and hand to agents, or `None` when it can't be
/// shown safely (fail closed). Removes userinfo (`user:token@`, `token@`,
/// percent-encoded or not), the query string and the fragment from scheme URLs
/// (`https://`, `ssh://`, `file://`, ...). An scp-style `user@host:path` keeps
/// its user: ssh authenticates with keys, so the name is not a credential and
/// the URL still clones. Local paths are returned unchanged. Anything ambiguous
/// returns `None`: an `@` after the authority (an unencoded `/`, `?` or `#`
/// inside a password would otherwise leak its tail), an scp user that could be
/// a token (see [`is_scp_user`]), an `@` in a local path, a host that isn't a
/// plain name or bracketed IPv6 literal, a non-numeric port, whitespace or
/// control characters, and `transport::address` remote-helper forms.
pub(crate) fn redact_origin_url(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    if let Some((scheme, rest)) = raw.split_once("://") {
        if !is_url_scheme(scheme) {
            return None;
        }
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        if tail.contains('@') {
            return None;
        }
        let host_port = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host_ok = if host_port.is_empty() {
            scheme.eq_ignore_ascii_case("file")
        } else {
            is_host_port(host_port)
        };
        if !host_ok {
            return None;
        }
        let path = &tail[..tail.find(['?', '#']).unwrap_or(tail.len())];
        return Some(format!("{scheme}://{host_port}{path}"));
    }
    if let Some((transport, _)) = raw.split_once("::") {
        if is_url_scheme(transport) {
            return None;
        }
    }
    // git's rule: a `:` before any `/` makes it scp-style `[user@]host:path`;
    // otherwise (and for a Windows drive like `C:\`) it is a local path.
    let first_colon = raw.find(':');
    let first_slash = raw.find('/');
    let scp_like = matches!((first_colon, first_slash), (Some(c), Some(s)) if c < s)
        || matches!((first_colon, first_slash), (Some(_), None));
    let windows_drive = raw.len() >= 3
        && raw.as_bytes()[0].is_ascii_alphabetic()
        && raw.as_bytes()[1] == b':'
        && matches!(raw.as_bytes()[2], b'\\' | b'/');
    if !scp_like || windows_drive {
        // `user:token@` can't be told apart from a directory name here.
        return (!raw.contains('@')).then(|| raw.to_string());
    }
    // The user ends at the first `@`, so it never contains `:` (`u:t@h:p`
    // parses as host `u` with an `@` in the path, and fails below).
    let (user, host_and_path) = match raw.find(['@', ':', '[']) {
        Some(i) if raw.as_bytes()[i] == b'@' => (Some(&raw[..i]), &raw[i + 1..]),
        _ => (None, raw),
    };
    let host_end = if host_and_path.starts_with('[') {
        host_and_path.find(']').map(|i| i + 1)?
    } else {
        host_and_path.find(':')?
    };
    let (host, colon_path) = host_and_path.split_at(host_end);
    let path = colon_path.strip_prefix(':')?;
    if path.contains('@') || !is_host(host) || user.is_some_and(|u| !is_scp_user(u)) {
        return None;
    }
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    let user = user.map_or(String::new(), |u| format!("{u}@"));
    Some(format!("{user}{host}:{path}"))
}

/// An scp-style ssh user that is safe to keep: a short plain name that doesn't
/// start like a known token. Anything else could be a token pasted where the
/// user goes, so the URL fails closed.
fn is_scp_user(user: &str) -> bool {
    const TOKEN_PREFIXES: [&str; 6] = ["ghp_", "gho_", "ghs_", "github_pat_", "glpat-", "xox"];
    let lower = user.to_ascii_lowercase();
    !user.is_empty()
        && user.len() <= 64
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'))
        && !TOKEN_PREFIXES.iter().any(|p| lower.starts_with(p))
}

fn is_url_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// `host` or `host:port`, where the port is all digits.
fn is_host_port(host_port: &str) -> bool {
    let (host, port) = if host_port.starts_with('[') {
        match host_port.find(']') {
            Some(i) => host_port.split_at(i + 1),
            None => return false,
        }
    } else {
        match host_port.find(':') {
            Some(i) => host_port.split_at(i),
            None => (host_port, ""),
        }
    };
    let port_ok = port.is_empty()
        || port
            .strip_prefix(':')
            .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()));
    port_ok && is_host(host)
}

/// A plain host name or a bracketed IPv6 literal (with an optional `%25` zone).
fn is_host(host: &str) -> bool {
    if let Some(literal) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        let (address, zone) = literal.split_once("%25").unwrap_or((literal, ""));
        return address.contains(':')
            && address
                .chars()
                .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'))
            && zone
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'));
    }
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'))
}

fn registry_paths(path: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    retry_descriptor_io(|| {
        std::fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    })
}

fn discover_managed_orgs_impl(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<Vec<OrgSummary>, String> {
    let orgs_dir = path_to_user_home(codesense_home, principal).join(ORGS_DIR);

    if !orgs_dir.exists() {
        return Ok(vec![]);
    }

    let entries = registry_paths(&orgs_dir)
        .map_err(|e| format!("cannot read orgs directory '{}': {}", orgs_dir.display(), e))?;

    let mut summaries = Vec::new();

    for path in entries {
        // Reject symlinked entries outright: descending one (whether at the
        // owner level or the legacy single-segment level) would let any
        // `.git`-bearing directory anywhere on disk surface as an org.
        // `symlink_metadata` distinguishes symlinks without following them.
        // Keep paths rather than DirEntry handles so the directory fd is closed.
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let file_type = metadata.file_type();
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }

        // SW-CR-1: an org slug is one or two `/`-separated segments. A dir
        // that is itself a git clone is a single-segment org (e.g. a
        // user-attached `notes/`); read its slug directly. Otherwise treat
        // this level as `{owner}` and look one deeper for `{repo}` clones
        // in the nested `orgs/{owner}/{repo}` layout.
        if path.join(".git").exists() {
            let Some(slug) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some(summary) = org_summary_at(&path, slug.to_string())? {
                summaries.push(summary);
            }
            continue;
        }

        let Some(owner) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let repo_entries = match registry_paths(&path) {
            Ok(entries) => entries,
            Err(error) if descriptor_exhausted(&error) => {
                return Err(format!("cannot read orgs directory '{}': too many open files (temporary; retry): {error}", path.display()));
            }
            Err(_) => continue,
        };
        for repo_path in repo_entries {
            // Same symlink guard at the repo level: a symlinked repo dir is
            // not a real clone and must not surface as an org either.
            let Ok(metadata) = std::fs::symlink_metadata(&repo_path) else {
                continue;
            };
            let repo_ft = metadata.file_type();
            if repo_ft.is_symlink() || !repo_ft.is_dir() {
                continue;
            }
            if !repo_path.join(".git").exists() {
                continue;
            }
            let Some(repo) = repo_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some(summary) = org_summary_at(&repo_path, format!("{}/{}", owner, repo))? {
                summaries.push(summary);
            }
        }
    }

    // SW-CR-1: skip any org whose slug fails the canonical `validate_org_slug`
    // gate downstream consumers all funnel through — a bare `<name>` or
    // `<owner>/<repo>`. Surfacing a rejected slug would let the frontend select
    // an org every install/sync call would then reject — worse UX than hiding
    // it. Filtered AFTER summary construction so we still attempted the
    // dir-walk (any logged warnings from there are preserved).
    // SLW-PT-19: the retired identity-keyed personal org (`_personal/_personal`)
    // stays on disk untouched (its data is sorted by hand later) but is never an
    // org: no nav row, no routines, no team discovery.
    summaries.retain(|s| {
        s.slug.split('/').next() != Some(crate::context_repo::scoped_key::PERSONAL_OWNER)
    });
    summaries.retain(|s| {
        let ok = crate::context_repo::org_slug::validate_org_slug(&s.slug).is_none();
        if !ok {
            log::warn!(
                target: "commands::org",
                "discover_local_orgs: hiding org {:?} (slug rejected by validator; awaiting migration)",
                s.slug
            );
        }
        ok
    });
    let pending_path =
        crate::context_repo::path_to_user_pending_org_creates_file(codesense_home, principal);
    let pending = crate::context_repo::pending_org_creates::list_pending(&pending_path)
        .map_err(|error| error.to_string())?;
    summaries.retain(|summary| !pending.contains_key(&summary.slug.to_ascii_lowercase()));
    summaries.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(summaries)
}

/// Build an [`OrgSummary`] for an org clone rooted at `path` with the
/// given `slug`. Returns `None` (skip) when `org.yml` is missing or
/// unparseable. F-38: `has_remote` / `remote_url` come from a single
/// git2 round-trip; `Remote::url()` is `None` for non-UTF-8 URLs while
/// `has_remote` stays true (the remote exists, we just can't surface
/// the URL as a String to the frontend).
fn org_summary_at(path: &Path, slug: String) -> Result<Option<OrgSummary>, String> {
    let bytes = match retry_descriptor_io(|| std::fs::read(path.join("org.yml"))) {
        Ok(bytes) => bytes,
        Err(error) if descriptor_exhausted(&error) => {
            return Err(format!(
                "cannot load org {slug}: too many open files (temporary; retry): {error}"
            ));
        }
        Err(_) => return Ok(None),
    };
    let Ok(raw) = serde_yaml::from_slice::<RawOrgYml>(&bytes) else {
        return Ok(None);
    };
    let display_name = raw.display_name;

    let (has_remote, remote_url) = origin_of(path);

    Ok(Some(OrgSummary {
        is_attached: false,
        availability_error: None,
        slug,
        display_name,
        has_remote,
        remote_url,
        org_root: path.to_string_lossy().to_string(),
    }))
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use crate::context_repo::{
        path_to_managed_org_root, scaffold_org_context_repo, ScaffoldOrgInputs,
    };
    use crate::domain::load_org;

    use super::*;

    use tempfile::TempDir;

    fn setup_codesense_home() -> TempDir {
        TempDir::new().expect("failed to create temp dir")
    }

    fn scaffold_test_org(codesense_home: &Path, github_user_id: u64, slug: &str) {
        let org_root =
            path_to_managed_org_root(codesense_home, &Principal::from(github_user_id), slug);
        std::fs::create_dir_all(&org_root).unwrap();
        let inputs = ScaffoldOrgInputs {
            slug: slug.to_string(),
            display_name: format!("Test Org {}", slug),
            created_at: chrono::Utc::now(),
        };
        scaffold_org_context_repo(&org_root, &inputs).unwrap();
    }

    // -- discover_local_orgs tests --

    #[test]
    fn the_retired_personal_org_is_never_discovered_and_stays_on_disk() {
        let home = setup_codesense_home();
        scaffold_test_org(home.path(), 42, "acme/api");
        scaffold_test_org(home.path(), 42, "_personal/_personal");
        let found = discover_local_orgs_impl(home.path(), &Principal::from(42u64)).unwrap();
        assert_eq!(
            found.iter().map(|o| o.slug.as_str()).collect::<Vec<_>>(),
            ["acme/api"]
        );
        assert!(path_to_managed_org_root(
            home.path(),
            &Principal::from(42u64),
            "_personal/_personal"
        )
        .join("org.yml")
        .exists());
    }

    #[test]
    fn discover_empty_orgs_dir_returns_empty_vec() {
        let home = setup_codesense_home();
        let result = discover_local_orgs_impl(home.path(), &Principal::from(12345u64)).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn discover_finds_scaffolded_org() {
        let home = setup_codesense_home();
        // SS-FU-12: org slug is the two-segment `owner/repo` form.
        scaffold_test_org(home.path(), 42, "test-org/api");

        let result = discover_local_orgs_impl(home.path(), &Principal::from(42u64)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].slug, "test-org/api");
        assert_eq!(result[0].display_name, "Test Org test-org/api");
        assert!(!result[0].has_remote);
        assert!(result[0].remote_url.is_none());
        assert!(!result[0].org_root.is_empty());
    }

    #[test]
    fn discover_finds_nested_owner_repo_org() {
        // SS-FU-12: org dirs live at `orgs/{owner}/{repo}`.
        let home = setup_codesense_home();
        scaffold_test_org(home.path(), 42, "acme/widgets-context");

        let result = discover_local_orgs_impl(home.path(), &Principal::from(42u64)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].slug, "acme/widgets-context");
        assert_eq!(result[0].display_name, "Test Org acme/widgets-context");
    }

    #[test]
    fn discover_handles_mixed_legacy_and_nested_layout() {
        // SW-CR-1: an org slug is one or two `/`-separated segments. A dir
        // that is itself a git clone surfaces as a single-segment org; the
        // nested `{owner}/{repo}` layout surfaces as a two-segment one. Both
        // forms are valid identities post-migration retirement (SW-CR-4), so
        // discovery must find both side by side.
        let home = setup_codesense_home();
        scaffold_test_org(home.path(), 42, "legacy--org");
        scaffold_test_org(home.path(), 42, "acme/widgets-context");

        let slugs: Vec<String> = discover_local_orgs_impl(home.path(), &Principal::from(42u64))
            .unwrap()
            .into_iter()
            .map(|s| s.slug)
            .collect();
        assert_eq!(slugs, vec!["acme/widgets-context", "legacy--org"]);
    }

    #[test]
    fn discover_skips_non_repo_directories() {
        let home = setup_codesense_home();
        let orgs_dir = path_to_user_home(home.path(), &Principal::from(42u64)).join(ORGS_DIR);
        std::fs::create_dir_all(orgs_dir.join("not-a-repo")).unwrap();

        let result = discover_local_orgs_impl(home.path(), &Principal::from(42u64)).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn discover_skips_symlinked_org_dirs() {
        // A symlinked owner-level (or legacy single-segment) dir under orgs/
        // would let a `.git`-bearing directory anywhere on disk surface as an
        // org — info disclosure. The walk must reject symlinks before descent.
        let home = setup_codesense_home();
        // Scaffold a real org elsewhere on disk to serve as the symlink target.
        let other_home = setup_codesense_home();
        scaffold_test_org(other_home.path(), 7, "real-target-org");
        let target =
            path_to_managed_org_root(other_home.path(), &Principal::from(7u64), "real-target-org");

        let orgs_dir = path_to_user_home(home.path(), &Principal::from(42u64)).join(ORGS_DIR);
        std::fs::create_dir_all(&orgs_dir).unwrap();
        // Owner-level symlink pointing at the real `.git`-bearing dir.
        std::os::unix::fs::symlink(&target, orgs_dir.join("smuggled-via-symlink")).unwrap();

        let result = discover_local_orgs_impl(home.path(), &Principal::from(42u64)).unwrap();
        assert!(
            result.is_empty(),
            "symlinked owner dir surfaced as an org: {result:?}"
        );
    }

    /// SLW-PT-18: prove the content-mobility QH fixture's ON-DISK org tree is
    /// boot-discoverable through the SAME natural-boot path production takes —
    /// `discover_local_orgs_impl` → `load_org`. This is the invariant the
    /// webdriver-only fixture (`tests/e2e-real/support/content-nav-fixture.ts`)
    /// depends on: a git clone under `orgs/{owner}/{repo}` with org/people/teams
    /// yml + a `{team}/content/` dir carrying authored (non-installed) docs. The
    /// prior QH attempt seeded this via a post-boot store swap, which never
    /// stamped `resolved_repository_paths` and never registered `workspaceRepos`
    /// — so `usePipelineTree` never committed. Discovery + load stamping the
    /// team's `source_repos` is what the natural boot uses to derive the nav's
    /// non-empty `contextRepoPath`, so assert both are present here.
    #[test]
    fn content_nav_fixture_tree_is_boot_discoverable() {
        use crate::context_repo::path_to_team_content_dir;

        let home = setup_codesense_home();
        let principal = Principal::from(1u64);
        let org_root = path_to_managed_org_root(home.path(), &principal, "alderco/primary");
        std::fs::create_dir_all(&org_root).unwrap();

        // Mirror the TS materializer: org/people/teams yml with TWO teams, each
        // carrying >=1 source_repo (the nav's hard requirement), plus authored
        // non-installed content docs under `{team}/content/`.
        std::fs::write(
            org_root.join("org.yml"),
            "slug: alderco/primary\ndisplay_name: Primary Org\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
        )
        .unwrap();
        std::fs::write(
            org_root.join("people.yml"),
            "people:\n  - github_user_id: 1\n    github_username: e2e\n    display_name: E2E\n",
        )
        .unwrap();
        std::fs::write(
            org_root.join("teams.yml"),
            "teams:\n  - name: source-team\n    source_repos:\n      - owner: alderco\n        repo: codesense\n  - name: target-team\n    source_repos:\n      - owner: alderco\n        repo: codesense-two\n",
        )
        .unwrap();
        for (team, doc) in [
            ("source-team", "movable-doc.md"),
            ("target-team", "target-landing.md"),
        ] {
            let content_dir = path_to_team_content_dir(&org_root, team);
            std::fs::create_dir_all(&content_dir).unwrap();
            std::fs::write(content_dir.join(doc), "# Authored\n\nDemo doc.\n").unwrap();
        }
        // Discovery requires a real `.git` clone with a HEAD.
        let repo = git2::Repository::init(&org_root).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("harness", "harness@codesense.test").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "seed", &tree, &[])
            .unwrap();

        // 1) Natural discovery finds the org under `users/1/orgs/{owner}/{repo}`.
        let summaries = discover_local_orgs_impl(home.path(), &principal).unwrap();
        assert_eq!(summaries.len(), 1, "expected exactly the seeded org");
        let summary = &summaries[0];
        assert_eq!(summary.slug, "alderco/primary");
        assert_eq!(summary.org_root, org_root.to_string_lossy());

        // 2) load_org yields both teams, each with its source_repo — the nav's
        //    `resolveTeamPipelineRepoPath` needs `source_repos[0]` to resolve a
        //    non-empty navRepoPath, and load_org_cmd derives contextRepoPath from
        //    the same source_repos.
        let loaded = load_org(summary.slug.clone(), &org_root, summary.remote_url.clone());
        assert!(
            loaded.file_errors.is_empty(),
            "org must load clean: {:?}",
            loaded.file_errors
        );
        let org = loaded.org.expect("org should compose");
        let names: Vec<&str> = org.teams.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["source-team", "target-team"]);
        assert_eq!(org.team("source-team").unwrap().source_repos.len(), 1);
        assert_eq!(
            org.team("source-team").unwrap().source_repos[0].repo,
            "codesense"
        );

        // 3) The authored content dir the scanner walks exists with the doc.
        let content_dir = path_to_team_content_dir(&org_root, "source-team");
        assert!(content_dir.join("movable-doc.md").is_file());
    }

    #[test]
    fn discover_detects_remote() {
        let home = setup_codesense_home();
        scaffold_test_org(home.path(), 42, "remote-org/api");

        let org_root =
            path_to_managed_org_root(home.path(), &Principal::from(42u64), "remote-org/api");
        let repo = git2::Repository::open(&org_root).unwrap();
        repo.remote("origin", "https://github.com/example/test.git")
            .unwrap();

        let result = discover_local_orgs_impl(home.path(), &Principal::from(42u64)).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].has_remote);
        assert_eq!(
            result[0].remote_url.as_deref(),
            Some("https://github.com/example/test.git"),
        );
    }

    // ---------- SV-GHI-1: attached checkouts report their origin ----------

    /// An attached, freshly created context repo (unborn, no origin).
    fn attached_org(home: &Path, root: &Path) -> Principal {
        let principal = Principal::local_guest();
        crate::commands::local_organization::create_at(home, &principal, "Acme", root).unwrap();
        principal
    }

    #[test]
    fn discover_reports_attached_origin() {
        let tmp = TempDir::new().unwrap();
        let (home, root) = (tmp.path().join("state"), tmp.path().join("acme"));
        let principal = attached_org(&home, &root);
        git2::Repository::open(&root)
            .unwrap()
            .remote("origin", "https://github.com/o/r.git")
            .unwrap();

        let found = discover_local_orgs_impl(&home, &principal).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].is_attached);
        assert!(found[0].has_remote);
        assert_eq!(
            found[0].remote_url.as_deref(),
            Some("https://github.com/o/r.git")
        );
    }

    #[test]
    fn discover_attached_origin_never_carries_a_token() {
        let tmp = TempDir::new().unwrap();
        let (home, root) = (tmp.path().join("state"), tmp.path().join("acme"));
        let principal = attached_org(&home, &root);
        git2::Repository::open(&root)
            .unwrap()
            .remote("origin", "https://bob:ghp_x@github.com/o/r.git") // trufflehog:ignore
            .unwrap();

        let found = discover_local_orgs_impl(&home, &principal).unwrap();
        assert_eq!(
            found[0].remote_url.as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert!(!format!("{:?}", found).contains("ghp_x"));
    }

    #[test]
    fn discover_attached_without_origin_is_local() {
        let tmp = TempDir::new().unwrap();
        let (home, root) = (tmp.path().join("state"), tmp.path().join("acme"));
        let principal = attached_org(&home, &root);

        let found = discover_local_orgs_impl(&home, &principal).unwrap();
        assert!(found[0].is_attached);
        assert!(!found[0].has_remote);
        assert_eq!(found[0].remote_url, None);
    }

    #[test]
    fn discover_attached_missing_root_reports_no_origin() {
        let tmp = TempDir::new().unwrap();
        let (home, root) = (tmp.path().join("state"), tmp.path().join("acme"));
        let principal = attached_org(&home, &root);
        git2::Repository::open(&root)
            .unwrap()
            .remote("origin", "https://github.com/o/r.git")
            .unwrap();
        std::fs::remove_dir_all(&root).unwrap();

        let found = discover_local_orgs_impl(&home, &principal).unwrap();
        assert!(found[0].availability_error.is_some());
        assert!(!found[0].has_remote);
        assert_eq!(found[0].remote_url, None);
    }

    #[test]
    fn origin_of_strips_userinfo_from_scheme_urls() {
        let tmp = TempDir::new().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        assert_eq!(origin_of(tmp.path()), (false, None));
        assert_eq!(origin_of(&tmp.path().join("missing")), (false, None));
        repo.remote("origin", "https://u:t@h/o/r.git").unwrap();
        assert_eq!(
            origin_of(tmp.path()),
            (true, Some("https://h/o/r.git".into()))
        );
        // A remote that can't be shown safely still counts as a remote.
        repo.remote_set_url("origin", "https://u:p/w@h/o/r.git")
            .unwrap();
        assert_eq!(origin_of(tmp.path()), (true, None));
    }

    #[test]
    fn redact_origin_url_removes_credentials_query_and_fragment() {
        let cases: &[(&str, Option<&str>)] = &[
            // https with user:pass, token-only userinfo, user only
            ("https://u:t@h/o/r.git", Some("https://h/o/r.git")),
            (
                "https://bob:ghp_x@github.com/o/r.git", // trufflehog:ignore
                Some("https://github.com/o/r.git"),
            ),
            (
                "https://ghp_x@github.com/o/r.git",
                Some("https://github.com/o/r.git"),
            ),
            (
                "https://x-access-token:ghs_x@github.com/o/r", // trufflehog:ignore
                Some("https://github.com/o/r"),
            ),
            (
                "https://bob@github.com/o/r.git",
                Some("https://github.com/o/r.git"),
            ),
            (
                "https://@github.com/o/r.git",
                Some("https://github.com/o/r.git"),
            ),
            (
                "https://github.com/o/r.git",
                Some("https://github.com/o/r.git"),
            ),
            ("http://h:8080/o/r", Some("http://h:8080/o/r")),
            (
                "HTTPS://Bob:t@GitHub.com/o/r",
                Some("HTTPS://GitHub.com/o/r"),
            ),
            // Query string and fragment
            (
                "https://h/o/r.git?access_token=t#frag",
                Some("https://h/o/r.git"),
            ),
            ("https://u:t@h/o/r.git#t", Some("https://h/o/r.git")),
            ("https://h?t", Some("https://h")),
            // Percent-encoded userinfo
            ("https://b%40x.com:p%3Aw%2Fd@h/o/r", Some("https://h/o/r")), // trufflehog:ignore
            ("https://%67hp_x@h/o/r", Some("https://h/o/r")),
            // A password with an unencoded `/`, `?` or `#` is ambiguous: fail closed.
            ("https://u:p/w@h/o/r", None),
            ("https://u:p?w@h/o/r", None),
            ("https://u:p#w@h/o/r", None),
            ("https://h/o/r@v", None),
            // ssh:// with user and port
            ("ssh://git@h/o/r", Some("ssh://h/o/r")),
            (
                "ssh://git@github.com:22/o/r.git",
                Some("ssh://github.com:22/o/r.git"),
            ),
            ("ssh://u:t@h:2222/~u/r.git", Some("ssh://h:2222/~u/r.git")),
            ("git+ssh://u@h/o/r", Some("git+ssh://h/o/r")),
            ("git://h/o/r.git", Some("git://h/o/r.git")),
            ("ssh://h:22x/o/r", None),
            ("ssh://h:/o/r", None),
            // scp-style: the ssh user stays (keys authenticate, not the name)
            ("git@github.com:o/r.git", Some("git@github.com:o/r.git")),
            ("gitea@h:o/r", Some("gitea@h:o/r")),
            (
                "org@vs-ssh.visualstudio.com:v3/org/p/r",
                Some("org@vs-ssh.visualstudio.com:v3/org/p/r"),
            ),
            ("github.com:o/r.git", Some("github.com:o/r.git")),
            ("git@h:o/r?t#f", Some("git@h:o/r")),
            // ...unless the user could be a token: fail closed
            ("ghp_x@github.com:o/r.git", None),
            ("gho_x@h:o/r", None),
            ("ghs_x@h:o/r", None),
            ("github_pat_x@h:o/r", None),
            ("glpat-x@h:o/r", None),
            ("xoxb-x@h:o/r", None),
            ("GHP_x@h:o/r", None),
            ("u:t@h:o/r", None),
            ("b%40x@h:o/r", None),
            ("@h:o/r", None),
            ("a@b@h:o/r", None),
            // IPv6 hosts
            ("https://u:t@[::1]/o/r", Some("https://[::1]/o/r")),
            (
                "ssh://git@[2001:db8::1]:2222/o/r",
                Some("ssh://[2001:db8::1]:2222/o/r"),
            ),
            (
                "https://[fe80::1%25en0]/o/r",
                Some("https://[fe80::1%25en0]/o/r"),
            ),
            ("git@[::1]:o/r", Some("git@[::1]:o/r")),
            ("u@[::1]:o/r", Some("u@[::1]:o/r")),
            ("https://[::1/o/r", None),
            ("https://[h]/o/r", None),
            ("git@[::1]o/r", None),
            // file:// and local paths
            ("file:///Users/me/repo", Some("file:///Users/me/repo")),
            ("file://u:t@/srv/repo", Some("file:///srv/repo")),
            ("file://server/share/repo", Some("file://server/share/repo")),
            ("/Users/me/repo.git", Some("/Users/me/repo.git")),
            ("../sibling", Some("../sibling")),
            ("./a:b", Some("./a:b")),
            ("~/repo", Some("~/repo")),
            ("C:\\repos\\r", Some("C:\\repos\\r")),
            ("C:/repos/r", Some("C:/repos/r")),
            // A local path with an `@` could hold `user:token@`: fail closed
            ("./u:t@h/r", None),
            ("/srv/ghp_x@h/r", None),
            ("repo@v1", None),
            ("C:\\u@h\\r", None),
            // Malformed: fail closed
            ("", None),
            ("https://", None),
            ("https:///o/r", None),
            ("https://h o/r", None),
            ("https://u:t@h/o/r\n", None),
            ("https://u:t@ho%2Fst/o/r", None),
            ("https://u:t@h\u{e9}/o/r", None),
            ("1ttp://h/o/r", None),
            ("/tmp/a://b", None),
            ("ext::ssh -o x h", None),
            ("fd::17", None),
            ("persistent-https::https://u:t@h/o/r", None),
        ];
        // An scp user of up to 64 characters stays; a longer one could be a token.
        let long_user = format!("{}@h:o/r", "a".repeat(64));
        let too_long_user = format!("{}@h:o/r", "a".repeat(65));
        let boundary = [
            (long_user.as_str(), Some(long_user.as_str())),
            (too_long_user.as_str(), None),
        ];
        for (raw, expected) in cases.iter().chain(&boundary) {
            let got = redact_origin_url(raw);
            assert_eq!(got.as_deref(), *expected, "{raw:?}");
            for secret in ["ghp_x", "ghs_x", ":t@", ":p", "%3A", "access_token"] {
                assert!(
                    !got.as_deref().unwrap_or("").contains(secret),
                    "{raw:?} leaked {secret:?}"
                );
            }
        }
    }

    // ---------- SW-FG-9: context-repo → connection binding resolution ----------
}

#[cfg(test)]
mod attachment_admission_tests {}
