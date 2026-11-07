// SPDX-License-Identifier: MIT
//! Scaffolding for a fresh org context repo: init git and seed registry
//! YAMLs without staging or committing. Idempotent and
//! process-wide-serialized.
//!
//! See B.2 (scaffold-org-context-repo) for the full contract.

// Downstream consumers of this module live in stories C.2 (first-launch
// bootstrap) and C.4 (create-new-org flow), which haven't landed yet. The
// B.2 surface is fully exercised by the inline `#[cfg(test)] mod tests`
// below, so dead-code is intentional at merge time.
#![allow(dead_code)]

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::domain::{OrgConfigFlags, RawOrgYml, RawPeopleYml, RawTeamsYml, RegistryFile};

// ---------- Types ----------

#[derive(Debug, Clone)]
pub struct ScaffoldOrgInputs {
    /// Stable registry identity, independent of the selected folder basename.
    pub slug: String,

    /// Human-readable display name, shown in codesense's UI. Written to
    /// `org.yml`'s `display_name` field.
    pub display_name: String,

    /// Creation timestamp. Always `Utc::now()` in production; tests pass a
    /// fixed fixture time. Written to `org.yml`'s `created_at`. Explicit
    /// parameter (not derived inside the function) so tests can produce
    /// deterministic YAML byte output.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaffoldOutcome {
    /// Fresh scaffold: `org_root` either didn't exist or was an empty
    /// directory. The `.git/` directory and the three registry YAMLs
    /// (`org.yml`, `people.yml`, `teams.yml`) were created without a commit.
    Created,

    /// `org_root` already existed with a `.git/` directory inside. A
    /// prior successful scaffold was assumed and nothing was touched.
    AlreadyExisted,
}

#[derive(Debug)]
pub enum ScaffoldError {
    /// `org_root` exists, is non-empty, and does NOT contain a `.git/`
    /// directory. Refusing to write into a directory we don't own.
    RefusingToClobber { path: PathBuf },

    /// `org_root` is a relative path. Callers must pass an absolute path.
    NotAbsolute { path: PathBuf },

    /// A directory creation failed (`org_root` itself).
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },

    /// YAML serialization of one of the four registry files failed.
    YamlSerialize {
        file: RegistryFile,
        source: serde_yaml::Error,
    },

    /// Writing one of the seeded files failed.
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },

    /// `git2` failed during init / index / tree / signature / commit.
    Git {
        step: &'static str,
        source: git2::Error,
    },
}

impl fmt::Display for ScaffoldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RefusingToClobber { path } => write!(
                f,
                "refusing to scaffold into non-empty non-git directory: {}",
                path.display()
            ),
            Self::NotAbsolute { path } => write!(
                f,
                "scaffold requires an absolute path, got: {}",
                path.display()
            ),
            Self::CreateDir { path, .. } => {
                write!(f, "failed to create directory: {}", path.display())
            }
            Self::YamlSerialize { file, .. } => {
                write!(f, "failed to serialize {}", file.filename())
            }
            Self::WriteFile { path, .. } => {
                write!(f, "failed to write file: {}", path.display())
            }
            Self::Git { step, .. } => {
                write!(f, "git operation failed during {}", step)
            }
        }
    }
}

impl std::error::Error for ScaffoldError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDir { source, .. } => Some(source),
            Self::WriteFile { source, .. } => Some(source),
            Self::YamlSerialize { source, .. } => Some(source),
            Self::Git { source, .. } => Some(source),
            Self::RefusingToClobber { .. } | Self::NotAbsolute { .. } => None,
        }
    }
}

// ---------- Constants ----------

static SCAFFOLD_LOCK: Mutex<()> = Mutex::new(());

// ---------- Internal helpers ----------

enum DirectoryState {
    DoesNotExist,
    Empty,
    AlreadyScaffolded,
    Occupied,
}

fn directory_state(org_root: &Path) -> DirectoryState {
    if !org_root.exists() {
        return DirectoryState::DoesNotExist;
    }
    if org_root.join(".git").exists() {
        return DirectoryState::AlreadyScaffolded;
    }
    match std::fs::read_dir(org_root) {
        Ok(mut iter) => {
            if iter.next().is_none() {
                DirectoryState::Empty
            } else {
                DirectoryState::Occupied
            }
        }
        Err(_) => DirectoryState::Occupied,
    }
}

fn write_yaml_file<T: serde::Serialize>(
    org_root: &Path,
    file: RegistryFile,
    value: &T,
) -> Result<(), ScaffoldError> {
    let yaml = serde_yaml::to_string(value)
        .map_err(|source| ScaffoldError::YamlSerialize { file, source })?;
    let path = org_root.join(file.filename());
    std::fs::write(&path, yaml).map_err(|source| ScaffoldError::WriteFile { path, source })?;
    Ok(())
}

fn write_registry_files(org_root: &Path, inputs: &ScaffoldOrgInputs) -> Result<(), ScaffoldError> {
    let raw_org = RawOrgYml {
        slug: inputs.slug.clone(),
        display_name: inputs.display_name.clone(),
        created_at: inputs.created_at,
        config_flags: OrgConfigFlags::default(),
        // SW-DX-1: a freshly scaffolded org has no tag registry (unopinionated).
        // A null raw value is `skip_serializing_if`-omitted, so org.yml stays
        // free of a spurious `tag_schema:` key.
        tag_schema: serde_yaml::Value::Null,
    };
    write_yaml_file(org_root, RegistryFile::OrgYml, &raw_org)?;

    let raw_people = RawPeopleYml { people: vec![] };
    write_yaml_file(org_root, RegistryFile::PeopleYml, &raw_people)?;

    let raw_teams = RawTeamsYml { teams: vec![] };
    write_yaml_file(org_root, RegistryFile::TeamsYml, &raw_teams)?;

    Ok(())
}

fn init_local_repository(org_root: &Path) -> Result<(), ScaffoldError> {
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    git2::Repository::init_opts(org_root, &opts).map_err(|source| ScaffoldError::Git {
        step: "init",
        source,
    })?;
    Ok(())
}

// ---------- Public API ----------

/// Scaffold a fresh org context repo on disk.
///
/// On-disk layout after a `Created` outcome:
///
/// ```text
/// {org_root}/
/// ├── .git/                  (HEAD → refs/heads/main)
/// ├── org.yml
/// ├── people.yml
/// └── teams.yml
/// ```
///
/// SW-OB-2: no `{team}/` is scaffolded — a fresh org has zero teams;
/// each team's directory is created on demand at create-team time.
///
/// Personal tiers are intentionally NOT scaffolded here — they live
/// out-of-repo under `{codesense_home}/users/{uid}/personal/{org_slug}/`
/// (AC-5) and are created lazily on first write; the resolver tolerates
/// their absence (AC-3).
///
/// **Idempotent.** Calling twice on the same root is safe — the second
/// call returns `AlreadyExisted` without touching state.
///
/// **Refuse-to-clobber.** If `org_root` exists and is non-empty without a
/// `.git/`, returns `RefusingToClobber`.
///
/// **Atomic scaffold.** On any error during scaffolding, `org_root` is
/// best-effort removed before the error is returned, so callers can
/// retry cleanly without manual cleanup.
///
/// **Process-wide mutex.** All scaffold calls serialize on a process-wide
/// mutex. Callers do not need their own synchronization.
///
/// **No remote.** Does NOT configure a git remote. Personal orgs get
/// remotes via B.11 `set_personal_org_remote`; new work orgs get remotes
/// wired by the caller after this function returns.
///
/// **Slug validation.** `inputs.slug` is written verbatim; callers must
/// pre-validate.
///
/// See B.2 (scaffold-org-context-repo) for rationale.
pub fn scaffold_org_context_repo(
    org_root: &Path,
    inputs: &ScaffoldOrgInputs,
) -> Result<ScaffoldOutcome, ScaffoldError> {
    // AC-5: reject relative paths up front. No cleanup needed — we
    // haven't touched the filesystem yet.
    if !org_root.is_absolute() {
        return Err(ScaffoldError::NotAbsolute {
            path: org_root.to_path_buf(),
        });
    }

    // AC-12: hold the mutex for the entire scaffold body.
    let _lock = SCAFFOLD_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // AC-6/AC-7: dispatch on directory state. Both short-circuit branches
    // touch nothing, so neither needs the AC-23 cleanup path.
    match directory_state(org_root) {
        DirectoryState::AlreadyScaffolded => {
            return Ok(ScaffoldOutcome::AlreadyExisted);
        }
        DirectoryState::Occupied => {
            return Err(ScaffoldError::RefusingToClobber {
                path: org_root.to_path_buf(),
            });
        }
        DirectoryState::DoesNotExist | DirectoryState::Empty => {
            // Fresh scaffold path below.
        }
    }

    std::fs::create_dir_all(org_root).map_err(|source| ScaffoldError::CreateDir {
        path: org_root.to_path_buf(),
        source,
    })?;

    // AC-23: atomic scaffold. Run the write/init steps inside a closure
    // so we can intercept ANY error and best-effort clean up `org_root`
    // before returning. Without this, a mid-scaffold failure strands the
    // directory: the next retry sees `Occupied` and returns
    // `RefusingToClobber`.
    let result = (|| -> Result<(), ScaffoldError> {
        write_registry_files(org_root, inputs)?;
        init_local_repository(org_root)?;
        Ok(())
    })();

    if let Err(err) = result {
        if let Err(cleanup_err) = std::fs::remove_dir_all(org_root) {
            log::warn!(
                "scaffold_org_context_repo: cleanup after error failed at {}: {}",
                org_root.display(),
                cleanup_err,
            );
        }
        return Err(err);
    }

    Ok(ScaffoldOutcome::Created)
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_inputs() -> ScaffoldOrgInputs {
        ScaffoldOrgInputs {
            slug: "alpha-corp--context".to_string(),
            display_name: "Alpha Corp".to_string(),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-04-14T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        }
    }

    /// Recursive walker — zero new dependencies. Returns every entry
    /// under `root` as a path relative to `root`. Sort at the call site
    /// before comparing (readdir order is not deterministic across
    /// filesystems).
    fn list_relative_paths(root: &Path) -> Vec<PathBuf> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if let Ok(rel) = path.strip_prefix(base) {
                    out.push(rel.to_path_buf());
                }
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    walk(base, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out
    }

    // ---------- Round-trip tests ----------

    #[test]
    fn scaffold_output_round_trips_through_a2_parser() {
        use crate::domain::{parse_org_yml, parse_people_yml, parse_teams_yml};

        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("alpha-corp--context");
        let inputs = ScaffoldOrgInputs {
            slug: "alpha-corp--context".to_string(),
            display_name: "Alpha Corp".to_string(),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-04-14T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        };

        let outcome = scaffold_org_context_repo(&org_root, &inputs).unwrap();
        assert_eq!(outcome, ScaffoldOutcome::Created);

        let org_bytes = std::fs::read(org_root.join("org.yml")).unwrap();
        let raw_org = parse_org_yml(&org_bytes).unwrap();
        assert_eq!(raw_org.slug, "alpha-corp--context");
        assert_eq!(raw_org.display_name, "Alpha Corp");

        let people_bytes = std::fs::read(org_root.join("people.yml")).unwrap();
        let raw_people = parse_people_yml(&people_bytes).unwrap();
        assert!(raw_people.people.is_empty());

        let teams_bytes = std::fs::read(org_root.join("teams.yml")).unwrap();
        let raw_teams = parse_teams_yml(&teams_bytes).unwrap();
        assert!(raw_teams.teams.is_empty());

        // SW-CR-3 AC-1: a brand-new scaffolded org has no manifest.yml on disk.
        assert!(
            !org_root.join("manifest.yml").exists(),
            "scaffold must not write manifest.yml"
        );
    }

    #[test]
    fn scaffolded_org_yml_does_not_contain_is_personal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("test-org");
        let inputs = ScaffoldOrgInputs {
            slug: "test-org".to_string(),
            display_name: "Test Org".to_string(),
            created_at: chrono::Utc::now(),
        };

        scaffold_org_context_repo(&org_root, &inputs).unwrap();

        let org_yml = std::fs::read_to_string(org_root.join("org.yml")).unwrap();
        assert!(
            !org_yml.contains("is_personal"),
            "org.yml should not contain is_personal after retirement"
        );
    }

    #[test]
    fn scaffold_output_parses_as_raw_serde_yaml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("raw-parse-check");
        scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();

        let org_text = std::fs::read_to_string(org_root.join("org.yml")).unwrap();
        let raw_org: RawOrgYml = serde_yaml::from_str(&org_text).unwrap();
        assert_eq!(raw_org.slug, "alpha-corp--context");

        let people_text = std::fs::read_to_string(org_root.join("people.yml")).unwrap();
        let raw_people: RawPeopleYml = serde_yaml::from_str(&people_text).unwrap();
        assert!(raw_people.people.is_empty());

        let teams_text = std::fs::read_to_string(org_root.join("teams.yml")).unwrap();
        let raw_teams: RawTeamsYml = serde_yaml::from_str(&teams_text).unwrap();
        assert!(raw_teams.teams.is_empty());
    }

    #[test]
    fn error_display_messages_contain_context() {
        let path = PathBuf::from("/tmp/nope");

        let err = ScaffoldError::RefusingToClobber { path: path.clone() };
        let msg = format!("{err}");
        assert!(msg.contains("/tmp/nope"), "got: {msg}");

        let err = ScaffoldError::NotAbsolute { path: path.clone() };
        let msg = format!("{err}");
        assert!(msg.contains("/tmp/nope"), "got: {msg}");

        let err = ScaffoldError::Git {
            step: "init",
            source: git2::Error::from_str("boom"),
        };
        let msg = format!("{err}");
        assert!(msg.contains("init"), "got: {msg}");
    }

    // ---------- Contract tests ----------

    #[test]
    fn scaffold_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("alpha");
        let inputs = fixture_inputs();

        let first = scaffold_org_context_repo(&org_root, &inputs).unwrap();
        assert_eq!(first, ScaffoldOutcome::Created);

        let mut listing_before = list_relative_paths(&org_root);
        listing_before.sort();

        let second = scaffold_org_context_repo(&org_root, &inputs).unwrap();
        assert_eq!(second, ScaffoldOutcome::AlreadyExisted);

        let mut listing_after = list_relative_paths(&org_root);
        listing_after.sort();
        assert_eq!(listing_before, listing_after);
    }

    #[test]
    fn scaffold_refuses_to_clobber_non_git_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("has-other-data");
        std::fs::create_dir_all(&org_root).unwrap();
        std::fs::write(org_root.join("random.txt"), "do not touch").unwrap();

        let err = scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap_err();
        assert!(matches!(err, ScaffoldError::RefusingToClobber { .. }));

        assert_eq!(
            std::fs::read_to_string(org_root.join("random.txt")).unwrap(),
            "do not touch"
        );
        assert!(!org_root.join(".git").exists());
        assert!(!org_root.join("org.yml").exists());
    }

    #[test]
    fn scaffold_into_empty_existing_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("empty");
        std::fs::create_dir_all(&org_root).unwrap();
        assert!(std::fs::read_dir(&org_root).unwrap().next().is_none());

        let outcome = scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();
        assert_eq!(outcome, ScaffoldOutcome::Created);
        assert!(org_root.join(".git").is_dir());
        assert!(org_root.join("org.yml").is_file());
    }

    #[test]
    fn scaffold_rejects_relative_paths() {
        let err =
            scaffold_org_context_repo(Path::new("relative/path"), &fixture_inputs()).unwrap_err();
        assert!(matches!(err, ScaffoldError::NotAbsolute { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn partial_failure_cleans_up_and_allows_retry() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("partial-fail");
        std::fs::create_dir_all(&org_root).unwrap();

        // Make org_root read+exec only (no write). `directory_state` still
        // returns `Empty` because `read_dir` works on a 0o555 directory,
        // but the subsequent `fs::write(org_root/org.yml, ...)` fails with
        // EACCES — exercising the cleanup path inside the write/init
        // closure.
        let mut perms = std::fs::metadata(&org_root).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&org_root, perms).unwrap();

        let err = scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap_err();
        assert!(
            matches!(err, ScaffoldError::WriteFile { .. }),
            "expected WriteFile from EACCES, got {err:?}",
        );

        // AC-23: cleanup ran. `remove_dir_all` needs write+exec on the
        // parent (tmp.path(), writable) to rmdir the empty child.
        assert!(
            !org_root.exists(),
            "cleanup should have removed org_root, but it still exists",
        );

        // Retry succeeds immediately — no manual rm -rf required.
        let outcome = scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();
        assert_eq!(outcome, ScaffoldOutcome::Created);
        assert!(org_root.join(".git").is_dir());
        assert!(org_root.join("org.yml").is_file());
    }

    // ---------- Git repo state tests ----------

    #[test]
    fn scaffold_pins_initial_branch_to_main() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("branch-check");
        scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();

        let repo = git2::Repository::open(&org_root).unwrap();
        let head = repo.find_reference("HEAD").unwrap();
        assert_eq!(head.symbolic_target(), Ok(Some("refs/heads/main")));
        assert!(!repo.is_bare());
    }

    #[test]
    fn scaffold_leaves_seed_files_uncommitted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("local-context");
        scaffold_org_context_repo(&root, &fixture_inputs()).unwrap();
        let repo = git2::Repository::open(&root).unwrap();
        assert_eq!(
            repo.head().err().unwrap().code(),
            git2::ErrorCode::UnbornBranch
        );
        assert!(repo.index().unwrap().is_empty());
        assert!(root.join("teams.yml").is_file());
        assert!(root.join("people.yml").is_file());
    }

    // ---------- Guard tests ----------

    #[test]
    fn scaffold_does_not_create_bmad_output_at_org_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("org");
        scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();

        // _bmad-output must NOT exist at org root — it belongs under
        // _team/{team}/repos/{owner}/{repo}/ per tiered-collaboration arch.
        assert!(
            !org_root.join("_bmad-output").exists(),
            "scaffold_org_context_repo must not create _bmad-output at org root"
        );
    }

    /// SW-LT-1 AC-5: the personal tier is out-of-repo by construction —
    /// scaffold must never create a `_personal/` inside the context repo
    /// (which would sync to teammates). Personal lives under
    /// `{codesense_home}/users/{uid}/personal/{org_slug}/`, created lazily
    /// on first write.
    #[test]
    fn scaffold_does_not_create_personal_inside_org_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("org");
        scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();

        assert!(
            !org_root.join("_personal").exists(),
            "scaffold must not create _personal/ inside the context repo"
        );
    }

    /// RF-31: the retired `_org/` reserved dir is no longer created on disk at
    /// scaffold time — `org.yml` is now the org-root marker, and no `_org/`
    /// skeleton remains.
    #[test]
    fn scaffold_does_not_create_org_shared_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("org");
        scaffold_org_context_repo(&org_root, &fixture_inputs()).unwrap();

        assert!(
            !org_root.join("_org").exists(),
            "scaffold must NOT create _org/ (retired by RF-31)"
        );
    }

    // ---------- Concurrency ----------

    #[test]
    fn concurrent_scaffolds_serialize() {
        use std::thread;

        let tmp = tempfile::TempDir::new().unwrap();
        let org_root = tmp.path().join("race");
        let inputs = fixture_inputs();

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let org_root = org_root.clone();
                let inputs = inputs.clone();
                thread::spawn(move || scaffold_org_context_repo(&org_root, &inputs))
            })
            .collect();

        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().unwrap().unwrap())
            .collect();

        let created_count = outcomes
            .iter()
            .filter(|o| **o == ScaffoldOutcome::Created)
            .count();
        let existed_count = outcomes
            .iter()
            .filter(|o| **o == ScaffoldOutcome::AlreadyExisted)
            .count();
        assert_eq!(created_count, 1);
        assert_eq!(existed_count, 3);

        let repo = git2::Repository::open(&org_root).unwrap();
        assert_eq!(
            repo.find_reference("HEAD").unwrap().symbolic_target(),
            Ok(Some("refs/heads/main"))
        );
        assert!(org_root.join("org.yml").is_file());
    }
}
