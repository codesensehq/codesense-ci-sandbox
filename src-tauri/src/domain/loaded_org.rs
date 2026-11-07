// SPDX-License-Identifier: MIT
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::org::Org;
use super::raw_yaml::{
    classify_domain_error, parse_org_yml, parse_people_yml, parse_teams_yml, ParseError,
    RawOrgComposition, RawOrgYml, RawPeopleYml, RawTeamsYml, RegistryFile,
};

/// Inject the runtime `org_root` path into a composed `Org`.
/// Called after YAML composition because `org_root` is deployment-dependent
/// and must not be stored in the YAML registry.
fn stamp_org_root(org: Option<Org>, org_root: &Path) -> Option<Org> {
    org.map(|mut o| {
        o.org_root = org_root.display().to_string();
        o
    })
}

/// Compose the domain `Org` from a fully-read `partial`, using `dir_slug` (the
/// on-disk org directory name) as the authoritative `Org.slug`.
///
/// Returns `None` without touching `errors` when a prior file-read stage
/// already failed (`errors` non-empty). Today composition can only fail with
/// `ParseError::Invariant` — parse/utf8 errors are caught earlier in the
/// file-read stage. If that contract is ever broken by a future refactor we
/// surface the error via `errors` (rather than panic inside a Tauri command
/// handler) with a real file path, so "Open to repair" doesn't point at a
/// directory.
fn compose_org(
    partial: &PartialOrg,
    remote_url: Option<String>,
    dir_slug: String,
    org_root: &Path,
    errors: &mut Vec<FileLoadError>,
) -> Option<Org> {
    if !errors.is_empty() {
        return None;
    }
    match RawOrgComposition::from_partial(partial, remote_url).and_then(|c| c.into_org(dir_slug)) {
        Ok(org) => Some(org),
        Err(parse_err) => {
            let composition_path = match &parse_err {
                ParseError::Invariant { source, .. } => classify_domain_error(source, org_root).1,
                ParseError::Yaml { file, .. } | ParseError::InvalidUtf8 { file } => {
                    log::error!(
                        target: "domain::loaded_org",
                        "composition produced a non-Invariant ParseError (contract violation): {parse_err:?}"
                    );
                    org_root.join(file.filename())
                }
            };
            errors.push(FileLoadError::from_parse_error(
                &composition_path,
                parse_err,
            ));
            None
        }
    }
}

// ---------- Types ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedOrg {
    pub slug: String,
    pub root: PathBuf,
    pub org: Option<Org>,
    pub partial: PartialOrg,
    pub file_errors: Vec<FileLoadError>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialOrg {
    pub org_yml: Option<RawOrgYml>,
    pub people_yml: Option<RawPeopleYml>,
    pub teams_yml: Option<RawTeamsYml>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileLoadError {
    pub file: RegistryFile,
    pub path: PathBuf,
    pub kind: FileLoadErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileLoadErrorKind {
    NotFound,
    ReadError,
    TransientReadError,
    Parse,
    Invariant,
}

impl FileLoadError {
    pub(crate) fn from_parse_error(path: &Path, err: ParseError) -> Self {
        let (file, kind) = match &err {
            ParseError::InvalidUtf8 { file } => (*file, FileLoadErrorKind::Parse),
            ParseError::Yaml { file, .. } => (*file, FileLoadErrorKind::Parse),
            ParseError::Invariant { file, .. } => (*file, FileLoadErrorKind::Invariant),
        };
        FileLoadError {
            file,
            path: path.to_path_buf(),
            kind,
            message: err.to_string(),
        }
    }

    fn read_error(file: RegistryFile, path: PathBuf, io_err: &std::io::Error) -> Self {
        // `io::Error`'s Display on most platforms already embeds the
        // OS-level error name (e.g. "Permission denied (os error 13)"),
        // so the kind is not duplicated in the message. We deliberately
        // do NOT format `io::ErrorKind` directly because it's
        // `#[non_exhaustive]` and its Debug representation is not a
        // stable API surface.
        FileLoadError {
            file,
            path,
            kind: if descriptor_exhausted(io_err) {
                FileLoadErrorKind::TransientReadError
            } else {
                FileLoadErrorKind::ReadError
            },
            message: if descriptor_exhausted(io_err) {
                format!("too many open files while loading registry (temporary; retry): {io_err}")
            } else {
                io_err.to_string()
            },
        }
    }

    fn not_found(file: RegistryFile, path: PathBuf) -> Self {
        FileLoadError {
            file,
            path,
            kind: FileLoadErrorKind::NotFound,
            message: "file not found".to_string(),
        }
    }
}

pub(crate) fn descriptor_exhausted(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE))
}

/// A startup/fork burst can temporarily consume the descriptor budget.
/// Keep retries bounded and preserve the OS error for callers to classify.
pub(crate) fn retry_descriptor_io<T>(
    mut operation: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    for delay in [20, 40, 80] {
        match operation() {
            Err(error) if descriptor_exhausted(&error) => {
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
            result => return result,
        }
    }
    operation()
}

// ---------- Loader ----------

/// Unified entry point for loading an org's registry files.
///
/// Always reads the working tree. Under the file-only model (SW-ID-8/9) the
/// working tree is the local source of truth: `git pull` merges remote changes
/// into it while preserving local WIP, so it already equals *last-pulled team
/// state + local pending edits* — exactly what the app should show, published or
/// not. `remote_url` is threaded straight through to `compose_org` so the
/// composed `Org` keeps its remote identity; it does not select the read source.
pub fn load_org(slug: impl Into<String>, org_root: &Path, remote_url: Option<String>) -> LoadedOrg {
    load_org_with_reader(slug.into(), org_root, remote_url, &mut |path| {
        std::fs::read(path)
    })
}

fn load_org_with_reader(
    slug: String,
    org_root: &Path,
    remote_url: Option<String>,
    read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
) -> LoadedOrg {
    let mut partial = PartialOrg::default();
    let mut errors = Vec::new();

    load_file(
        org_root.join("org.yml"),
        RegistryFile::OrgYml,
        parse_org_yml,
        &mut partial.org_yml,
        &mut errors,
        read,
    );

    load_file(
        org_root.join("people.yml"),
        RegistryFile::PeopleYml,
        parse_people_yml,
        &mut partial.people_yml,
        &mut errors,
        read,
    );

    load_file(
        org_root.join("teams.yml"),
        RegistryFile::TeamsYml,
        parse_teams_yml,
        &mut partial.teams_yml,
        &mut errors,
        read,
    );

    // Try full composition iff every file-read stage succeeded.
    let org = compose_org(&partial, remote_url, slug.clone(), org_root, &mut errors);
    let org = stamp_org_root(org, org_root);

    LoadedOrg {
        slug,
        root: org_root.to_path_buf(),
        org,
        partial,
        file_errors: errors,
    }
}

fn load_file<T, F>(
    path: PathBuf,
    file: RegistryFile,
    parser: F,
    dest: &mut Option<T>,
    errors: &mut Vec<FileLoadError>,
    read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
) where
    F: FnOnce(&[u8]) -> Result<T, ParseError>,
{
    match retry_descriptor_io(|| read(&path)) {
        Ok(bytes) => match parser(&bytes) {
            Ok(parsed) => *dest = Some(parsed),
            Err(parse_err) => {
                errors.push(FileLoadError::from_parse_error(&path, parse_err));
            }
        },
        Err(io_err) if io_err.kind() == std::io::ErrorKind::NotFound => {
            errors.push(FileLoadError::not_found(file, path));
        }
        Err(io_err) => {
            errors.push(FileLoadError::read_error(file, path, &io_err));
        }
    }
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::DomainError;
    use std::fs;
    use tempfile::TempDir;

    // Canonical valid fixture bodies.
    const VALID_ORG: &str = r#"
slug: alpha-corp
display_name: Alpha Corp
created_at: "2026-01-01T00:00:00Z"
"#;
    const VALID_PEOPLE: &str = r#"
people:
  - github_user_id: 1
    github_username: alice
    display_name: Alice
  - github_user_id: 2
    github_username: bob
"#;
    const VALID_TEAMS: &str = r#"
teams:
  - name: backend
    source_repos:
      - owner: alderco
        repo: codesense
"#;

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    fn write_all_valid(dir: &Path) {
        write(dir, "org.yml", VALID_ORG);
        write(dir, "people.yml", VALID_PEOPLE);
        write(dir, "teams.yml", VALID_TEAMS);
    }

    #[test]
    fn ordinary_io_errors_are_not_retried() {
        let mut attempts = 0;
        let result: std::io::Result<()> = retry_descriptor_io(|| {
            attempts += 1;
            Err(std::io::Error::from_raw_os_error(libc::EACCES))
        });
        assert_eq!(attempts, 1);
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EACCES));
    }

    #[test]
    fn descriptor_exhaustion_retries_and_recovers_the_org() {
        for errno in [libc::EMFILE, libc::ENFILE] {
            let td = TempDir::new().unwrap();
            write_all_valid(td.path());
            let mut attempts = 0;
            let loaded = load_org_with_reader("alpha-corp".into(), td.path(), None, &mut |path| {
                attempts += 1;
                if attempts <= 2 {
                    Err(std::io::Error::from_raw_os_error(errno))
                } else {
                    fs::read(path)
                }
            });
            assert!(loaded.org.is_some(), "{:?}", loaded.file_errors);
            assert_eq!(attempts, 5);
        }
    }

    #[test]
    fn exhausted_registry_reports_clearly_and_next_load_recovers() {
        let td = TempDir::new().unwrap();
        write_all_valid(td.path());
        let mut attempts = 0;
        let loaded = load_org_with_reader("alpha-corp".into(), td.path(), None, &mut |_| {
            attempts += 1;
            Err(std::io::Error::from_raw_os_error(libc::EMFILE))
        });
        assert_eq!(attempts, 12, "four attempts per registry file");
        assert!(loaded.org.is_none());
        assert!(loaded
            .file_errors
            .iter()
            .all(|e| e.message.contains("too many open files")
                && e.kind == FileLoadErrorKind::TransientReadError));
        let recovered = load_org("alpha-corp", td.path(), None);
        assert!(recovered.org.is_some(), "{:?}", recovered.file_errors);
    }

    #[test]
    fn all_clean() {
        let td = TempDir::new().unwrap();
        write_all_valid(td.path());
        let loaded = load_org("alpha-corp", td.path(), Some("git@x".into()));
        assert!(
            loaded.file_errors.is_empty(),
            "expected no errors: {:?}",
            loaded.file_errors
        );
        let org = loaded.org.expect("org should be Some");
        assert_eq!(org.slug, "alpha-corp");
        assert_eq!(org.people.len(), 2);
        assert_eq!(org.teams.len(), 1);
        assert_eq!(org.remote_url.as_deref(), Some("git@x"));
    }

    #[test]
    fn loader_ignores_lingering_manifest_yml() {
        // SW-CR-3 AC-2: a context repo still carrying an old `manifest.yml` on
        // disk loads cleanly — the loader stops reading the file (even an
        // intentionally-malformed one is a no-op).
        let td = TempDir::new().unwrap();
        write_all_valid(td.path());
        write(td.path(), "manifest.yml", "stale: yes\n");
        let loaded = load_org("alpha-corp", td.path(), None);
        assert!(
            loaded.file_errors.is_empty(),
            "manifest.yml must not surface an error: {:?}",
            loaded.file_errors
        );
        assert!(loaded.org.is_some());

        // Even a malformed on-disk manifest.yml is ignored (loader never reads it).
        write(td.path(), "manifest.yml", ":::garbage:::");
        let loaded = load_org("alpha-corp", td.path(), None);
        assert!(
            loaded.file_errors.is_empty(),
            "manifest.yml must not surface an error: {:?}",
            loaded.file_errors
        );
        assert!(loaded.org.is_some());
    }

    #[test]
    fn no_synthesized_team_beyond_teams_yml() {
        // SW-LT-7: the loader adds NO team beyond what `teams.yml` lists — even
        // if a `_team/org-root/` content dir exists, it is not synthesized into
        // a team, and a normal team's chain gains no implicit tail.
        let td = TempDir::new().unwrap();
        write_all_valid(td.path());
        fs::create_dir_all(td.path().join("org-root").join("install")).unwrap();
        let org = load_org("alpha-corp", td.path(), None)
            .org
            .expect("org should be Some");
        assert!(
            org.team("org-root").is_none(),
            "no team is synthesized from a content dir (SW-LT-7)"
        );
        assert_eq!(org.team_chain("backend"), vec!["backend".to_string()]);
    }

    #[test]
    fn org_yml_absent_fails() {
        let td = TempDir::new().unwrap();
        write(td.path(), "people.yml", VALID_PEOPLE);
        write(td.path(), "teams.yml", VALID_TEAMS);
        let loaded = load_org("alpha-corp", td.path(), None);
        assert!(loaded.org.is_none());
        assert_eq!(loaded.file_errors.len(), 1);
        let err = &loaded.file_errors[0];
        assert_eq!(err.file, RegistryFile::OrgYml);
        assert_eq!(err.kind, FileLoadErrorKind::NotFound);
        assert!(err.path.ends_with("org.yml"));
    }

    #[test]
    fn teams_yml_parse_error() {
        let td = TempDir::new().unwrap();
        write(td.path(), "org.yml", VALID_ORG);
        write(td.path(), "people.yml", VALID_PEOPLE);
        // A malformed struct shape: teams must be a list, not a scalar.
        write(td.path(), "teams.yml", "teams: not-a-list");
        let loaded = load_org("alpha-corp", td.path(), None);
        assert!(loaded.org.is_none());
        assert!(loaded.partial.org_yml.is_some());
        assert!(loaded.partial.people_yml.is_some());
        assert!(loaded.partial.teams_yml.is_none());
        assert_eq!(loaded.file_errors.len(), 1);
        assert_eq!(loaded.file_errors[0].file, RegistryFile::TeamsYml);
        assert_eq!(loaded.file_errors[0].kind, FileLoadErrorKind::Parse);
    }

    #[test]
    fn parent_team_cycle_classified_to_teams() {
        // SLW-IN-3 AC-1: a cyclic parent_team chain (alpha→beta→alpha) makes the
        // whole org fail to load with a single teams.yml Invariant error naming
        // a team in the cycle — surfaced through the same pipeline as
        // duplicate-team / unknown-user, so "Open to repair" points at teams.yml.
        let td = TempDir::new().unwrap();
        write(td.path(), "org.yml", VALID_ORG);
        write(td.path(), "people.yml", "people: []");
        write(
            td.path(),
            "teams.yml",
            r#"
teams:
  - name: alpha
    parent_team: beta
  - name: beta
    parent_team: alpha
"#,
        );
        let loaded = load_org("alpha-corp", td.path(), None);
        assert!(loaded.org.is_none());
        assert_eq!(loaded.file_errors.len(), 1);
        let err = &loaded.file_errors[0];
        assert_eq!(err.file, RegistryFile::TeamsYml);
        assert_eq!(err.kind, FileLoadErrorKind::Invariant);
        assert!(err.path.ends_with("teams.yml"));
        assert!(
            err.message.contains("alpha") && err.message.contains("cycle"),
            "message should name the cycle: {}",
            err.message
        );
    }

    #[test]
    fn dangling_parent_team_classified_to_teams() {
        // SLW-IN-3 AC-2: a parent_team naming an absent team is a typed load
        // error (was a silent resolve-to-absent-dir), naming the missing parent.
        let td = TempDir::new().unwrap();
        write(td.path(), "org.yml", VALID_ORG);
        write(td.path(), "people.yml", "people: []");
        write(
            td.path(),
            "teams.yml",
            r#"
teams:
  - name: alpha
    parent_team: ghost
"#,
        );
        let loaded = load_org("alpha-corp", td.path(), None);
        assert!(loaded.org.is_none());
        assert_eq!(loaded.file_errors.len(), 1);
        let err = &loaded.file_errors[0];
        assert_eq!(err.file, RegistryFile::TeamsYml);
        assert_eq!(err.kind, FileLoadErrorKind::Invariant);
        assert!(err.path.ends_with("teams.yml"));
        assert!(
            err.message.contains("ghost"),
            "message should name the missing parent: {}",
            err.message
        );
    }

    #[test]
    fn valid_parent_team_chain_loads_clean() {
        // Characterization: a well-formed parent chain (child → root) loads with
        // no errors — SLW-IN-3 validation only rejects malformed chains.
        let td = TempDir::new().unwrap();
        write(td.path(), "org.yml", VALID_ORG);
        write(td.path(), "people.yml", "people: []");
        write(
            td.path(),
            "teams.yml",
            r#"
teams:
  - name: platform
  - name: child
    parent_team: platform
"#,
        );
        let loaded = load_org("alpha-corp", td.path(), None);
        assert!(
            loaded.file_errors.is_empty(),
            "expected clean load: {:?}",
            loaded.file_errors
        );
        let org = loaded.org.expect("valid chain should compose");
        assert_eq!(org.teams.len(), 2);
    }

    #[test]
    fn invariant_duplicate_person_classified_to_people() {
        let td = TempDir::new().unwrap();
        write(td.path(), "org.yml", VALID_ORG);
        write(
            td.path(),
            "people.yml",
            r#"
people:
  - github_user_id: 1
    github_username: alice
  - github_user_id: 1
    github_username: alice2
"#,
        );
        write(
            td.path(),
            "teams.yml",
            r#"
teams: []
"#,
        );
        let loaded = load_org("alpha-corp", td.path(), None);
        assert!(loaded.org.is_none());
        assert_eq!(loaded.file_errors.len(), 1);
        let err = &loaded.file_errors[0];
        assert_eq!(err.file, RegistryFile::PeopleYml);
        assert_eq!(err.kind, FileLoadErrorKind::Invariant);
        assert!(err.path.ends_with("people.yml"));
    }

    #[test]
    fn invariant_empty_slug_classified_to_org() {
        // The authoritative slug is the dir name (the param), so a blank dir
        // slug is what must surface the EmptyField invariant — org.yml::slug
        // is informational only.
        let td = TempDir::new().unwrap();
        write(td.path(), "org.yml", VALID_ORG);
        write(td.path(), "people.yml", "people: []");
        write(td.path(), "teams.yml", "teams: []");
        let loaded = load_org("   ", td.path(), None);
        assert!(loaded.org.is_none());
        let err = &loaded.file_errors[0];
        assert_eq!(err.file, RegistryFile::OrgYml);
        assert_eq!(err.kind, FileLoadErrorKind::Invariant);
        assert!(err.path.ends_with("org.yml"));
        // The DomainError source should be EmptyField { field: "slug" }.
        // We don't have the source on FileLoadError directly, but the message
        // comes from ParseError::Display which embeds the DomainError Display.
        assert!(err.message.contains("slug"));
    }

    #[test]
    fn org_slug_derives_from_dir_not_drifted_yaml() {
        // Drift regression: org.yml::slug holds a short form ("codesense")
        // while the dir name is the URL-normalized slug. The loader's slug arg
        // (dir name) must win for Org.slug.
        let td = TempDir::new().unwrap();
        write(
            td.path(),
            "org.yml",
            r#"
slug: codesense
display_name: Alpha Corp
created_at: "2026-01-01T00:00:00Z"
"#,
        );
        write(td.path(), "people.yml", "people: []");
        write(td.path(), "teams.yml", "teams: []");
        let loaded = load_org("acme--team-context", td.path(), None);
        let org = loaded.org.expect("org should compose despite slug drift");
        assert_eq!(org.slug, "acme--team-context");
    }

    #[test]
    fn malformed_people_file_is_parse_error() {
        let td = TempDir::new().unwrap();
        write(td.path(), "org.yml", VALID_ORG);
        // `people` must be a sequence; a scalar is a shape mismatch.
        write(td.path(), "people.yml", "people: not-a-list");
        write(td.path(), "teams.yml", VALID_TEAMS);
        let loaded = load_org("alpha-corp", td.path(), None);
        assert!(loaded.org.is_none());
        assert_eq!(loaded.file_errors.len(), 1);
        assert_eq!(loaded.file_errors[0].file, RegistryFile::PeopleYml);
        assert_eq!(loaded.file_errors[0].kind, FileLoadErrorKind::Parse);
    }

    #[test]
    fn io_error_mapping_preserves_message() {
        // Direct unit test of the ReadError mapping path — avoids flaky
        // chmod-based tests.
        let fake = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "simulated-denied");
        let fle =
            FileLoadError::read_error(RegistryFile::OrgYml, PathBuf::from("/tmp/org.yml"), &fake);
        assert_eq!(fle.kind, FileLoadErrorKind::ReadError);
        assert!(fle.message.contains("simulated-denied"));
    }

    #[test]
    fn parse_error_source_returns_domain_error() {
        // Verifies ParseError::source() exposes the inner DomainError for
        // the Invariant variant, so downstream callers can downcast and
        // pattern-match instead of string-matching the Display output.
        use std::error::Error as _;

        let source = DomainError::DuplicateTeam("backend".into());
        let parse_err = ParseError::Invariant {
            file: RegistryFile::TeamsYml,
            source: source.clone(),
        };
        let inner = parse_err.source().expect("Invariant has a source");
        let downcast = inner
            .downcast_ref::<DomainError>()
            .expect("source downcasts to DomainError");
        assert_eq!(*downcast, source);
    }

    // ---- SW-ID-9 dispatcher tests ----
    //
    // `load_org` always reads the working tree; `remote_url` is threaded through
    // for identity only and no longer selects a git-origin read.

    #[test]
    fn read_source_is_independent_of_remote_url() {
        // `remote_url` selects identity, not read source: `None` and `Some(url)`
        // read the same working tree and compose the same roster/teams over it.
        let td = TempDir::new().unwrap();
        write_all_valid(td.path());
        let local = load_org("alpha-corp", td.path(), None);
        let team = load_org("alpha-corp", td.path(), Some("git@remote".to_string()));
        assert!(local.file_errors.is_empty() && team.file_errors.is_empty());
        let local_org = local.org.expect("local-only org loads");
        let team_org = team.org.expect("team-synced org loads");
        assert_eq!(local_org.people.len(), team_org.people.len());
        assert_eq!(local_org.teams.len(), team_org.teams.len());
        // Only the remote identity differs.
        assert_eq!(local_org.remote_url, None);
        assert_eq!(team_org.remote_url.as_deref(), Some("git@remote"));
    }

    #[test]
    fn remote_url_present_still_reads_working_tree() {
        // A team-synced org (`remote_url = Some`) reads the working tree, not a
        // git ref — no git repo is needed at `org_root` — and the remote
        // identity is preserved on the composed `Org`.
        let td = TempDir::new().unwrap();
        write_all_valid(td.path());
        let loaded = load_org("alpha-corp", td.path(), Some("git@remote".to_string()));
        assert!(
            loaded.org.is_some(),
            "org should be Some: {:?}",
            loaded.file_errors
        );
        assert!(loaded.file_errors.is_empty());
        assert_eq!(
            loaded.org.unwrap().remote_url.as_deref(),
            Some("git@remote")
        );
    }

    // AC-1: for a team-synced org, an uncommitted working-tree edit is what
    // `load_org` returns — even when `origin/HEAD` still holds the pre-edit
    // registry. This is the durability guarantee the story exists to deliver.
    #[test]
    fn team_synced_org_reads_working_tree_edit_over_origin_head() {
        let tmp = TempDir::new().unwrap();

        // Bare "remote" whose registry has two people (alice + bob), then a
        // non-bare clone so `origin/HEAD` is set and the files are checked out.
        let bare_path = tmp.path().join("remote.git");
        let bare_repo = git2::Repository::init_bare(&bare_path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let mut tb = bare_repo.treebuilder(None).unwrap();
        for (name, body) in [
            ("org.yml", VALID_ORG),
            ("people.yml", VALID_PEOPLE),
            ("teams.yml", VALID_TEAMS),
        ] {
            tb.insert(name, bare_repo.blob(body.as_bytes()).unwrap(), 0o100644)
                .unwrap();
        }
        let tree_oid = tb.write().unwrap();
        let tree = bare_repo.find_tree(tree_oid).unwrap();
        bare_repo
            .commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        bare_repo.set_head("refs/heads/main").unwrap();

        let local_path = tmp.path().join("local");
        git2::build::RepoBuilder::new()
            .bare(false)
            .clone(&format!("file://{}", bare_path.display()), &local_path)
            .unwrap();

        // Working-tree-only edit (uncommitted, unpublished): drop bob so the
        // working tree (one person) diverges from `origin/HEAD` (two people).
        let edited_people =
            "people:\n  - github_user_id: 1\n    github_username: alice\n    display_name: Alice\n";
        fs::write(local_path.join("people.yml"), edited_people).unwrap();

        let loaded = load_org("alpha-corp", &local_path, Some("file://remote".to_string()));
        let org = loaded.org.expect("org should load from the working tree");
        // Working-tree edit wins over the two-person `origin/HEAD` state.
        assert_eq!(
            org.people.len(),
            1,
            "expected the edited working-tree roster"
        );
        assert_eq!(org.slug, "alpha-corp");
        assert_eq!(org.teams.len(), 1);
        assert_eq!(org.remote_url.as_deref(), Some("file://remote"));
    }
}
