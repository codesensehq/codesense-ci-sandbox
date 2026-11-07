// SPDX-License-Identifier: MIT
//! Adding a source repo to a team, and binding a local clone to a tracked repo.
//!
//! This is the one home of the add-repo rules — how a typed URL or a picked
//! folder becomes a repo identity, when an add is a duplicate, and when a clone
//! may be bound to a repo. The app's add and attach forms (through the Tauri
//! commands in [`super::attachments`]) and the MCP setup tools all call these
//! functions, so the rules cannot drift between the UI and an agent.
//!
//! Roots resolve only through the existing resolvers (`path_to_org_root`,
//! `load_org`, the attachments store). Nothing here clones or fetches: a clone
//! is made with git by the user or an agent, then bound here. Every filesystem
//! and git read runs on the blocking pool.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::content_engine::sync::WaveCoordinator;
use crate::context_repo::{normalize_source_repo_url, RegistryEdit};
use crate::domain::{Attachment, Principal, SourceRepoSlug};
use crate::mcp::state::ActiveTeamState;

use super::attachments::{reconcile_after_upsert, upsert_attachment_record};

/// Emitted after a local clone is bound to, or unbound from, a source repo, so
/// views that show where a repo lives on disk re-read the attachments — however
/// the change was made (a form or an agent).
pub const SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT: &str =
    super::attachments::SOURCE_ATTACHMENTS_CHANGED_EVENT;

/// What to add: a remote (`https://…`, `git@…`, `host/owner/repo`, or the
/// GitHub shorthand `owner/repo`), or a folder already on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AddRepoInput {
    Url { url: String },
    LocalPath { path: String },
}

/// A repo tracked on a team, with the local clone bound to it when there is one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedSourceRepo {
    pub owner: String,
    pub repo: String,
    pub provider: String,
    pub host: String,
    pub remote_key: String,
    /// The bound local clone, when this call bound one.
    pub local_path: Option<String>,
    /// The clone's remote that names this repo (`upstream` in a fork
    /// workflow, usually `origin`), when the clone has one.
    pub remote_name: Option<String>,
    /// That remote's URL.
    pub remote_url: Option<String>,
}

impl TrackedSourceRepo {
    fn new(slug: &SourceRepoSlug, clone: Option<&PickedClone>) -> Self {
        Self {
            owner: slug.owner.clone(),
            repo: slug.repo.clone(),
            provider: slug.provider.clone(),
            host: slug.canonical_host().to_string(),
            remote_key: slug.remote_key(),
            local_path: clone.map(|c| c.path.clone()),
            remote_name: clone.and_then(|c| c.remote.as_ref().map(|(name, _)| name.clone())),
            remote_url: clone.and_then(|c| c.remote.as_ref().map(|(_, url)| url.clone())),
        }
    }
}

/// Why an add or a bind was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AddRepoError {
    InvalidInput {
        reason: String,
    },
    UnknownTeam {
        org: String,
        team: String,
    },
    AlreadyTracked {
        owner: String,
        repo: String,
        team: String,
    },
    NotTracked {
        repo: String,
        team: String,
    },
    NotADirectory {
        path: String,
    },
    NotAGitRepository {
        path: String,
    },
    /// The folder is inside a git working tree but is not its root (or is its
    /// `.git` directory); `root` is the folder to pick instead.
    NotRepoRoot {
        path: String,
        root: String,
    },
    NoOrigin {
        path: String,
    },
    UnsupportedRemote {
        url: String,
        reason: String,
    },
    /// None of the clone's remotes names the repo being bound. `expected` is
    /// that repo's `host/owner/repo`; `actual` is what the clone's preferred
    /// remote names.
    OriginMismatch {
        expected: String,
        actual: String,
    },
    Failure {
        message: String,
    },
}

impl std::fmt::Display for AddRepoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput { reason } => write!(f, "{reason}"),
            Self::UnknownTeam { org, team } => {
                write!(f, "{org} has no active team named {team}")
            }
            Self::AlreadyTracked { owner, repo, team } => {
                write!(f, "{owner}/{repo} is already tracked on team {team}")
            }
            Self::NotTracked { repo, team } => {
                write!(f, "{repo} is not one of team {team}'s source repos")
            }
            Self::NotADirectory { path } => write!(f, "{path} is not a folder"),
            Self::NotAGitRepository { path } => write!(
                f,
                "{path} is not a git repository — pick the folder you cloned with git"
            ),
            Self::NotRepoRoot { path, root } => write!(
                f,
                "{path} is inside the git repository at {root} — pick {root} itself"
            ),
            Self::NoOrigin { path } => write!(
                f,
                "{path} has no git remote, so there is no way to tell which repo it is"
            ),
            Self::UnsupportedRemote { url, reason } => {
                write!(f, "\"{url}\" is not a supported git remote: {reason}")
            }
            Self::OriginMismatch { expected, actual } => write!(
                f,
                "that clone is {actual}, and none of its remotes is {expected} — pick a clone of {expected}"
            ),
            Self::Failure { message } => write!(f, "{message}"),
        }
    }
}

/// The wire form of an [`AddRepoError`] for the app: its `kind` and fields,
/// plus a readable `message`.
#[derive(Debug, Clone, Serialize)]
pub struct AddRepoFailure {
    #[serde(flatten)]
    pub error: AddRepoError,
    pub message: String,
}

impl From<AddRepoError> for AddRepoFailure {
    fn from(error: AddRepoError) -> Self {
        let message = error.to_string();
        Self { error, message }
    }
}

fn failure(message: impl Into<String>) -> AddRepoError {
    AddRepoError::Failure {
        message: message.into(),
    }
}

/// Run `f` on the blocking pool, keeping its typed refusal.
async fn blocking<T, F>(label: &'static str, f: F) -> Result<T, AddRepoError>
where
    F: FnOnce() -> Result<T, AddRepoError> + Send + 'static,
    T: Send + 'static,
{
    super::await_blocking(label, move || Ok(f()))
        .await
        .map_err(failure)?
}

// ---- identity rules ---------------------------------------------------------

/// The repo a typed URL names. `owner/repo` is GitHub shorthand; anything else
/// must identify a Git host and owner/repo.
pub(crate) fn slug_from_url(input: &str) -> Result<SourceRepoSlug, AddRepoError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AddRepoError::InvalidInput {
            reason: "the repo URL is empty".to_string(),
        });
    }
    let segments: Vec<&str> = trimmed.split('/').collect();
    let github_shorthand =
        !trimmed.contains(':') && segments.len() == 2 && !segments[0].contains('.');
    let remote = if github_shorthand {
        format!("https://github.com/{trimmed}")
    } else {
        trimmed.to_string()
    };
    slug_from_remote(&remote)
}

/// The repo a git remote URL points at.
fn slug_from_remote(remote: &str) -> Result<SourceRepoSlug, AddRepoError> {
    let unsupported = |reason: String| AddRepoError::UnsupportedRemote {
        url: remote.to_string(),
        reason,
    };
    let normalized = normalize_source_repo_url(remote).map_err(|e| unsupported(e.to_string()))?;
    let host = normalized
        .normalized_url
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let provider = crate::commands::forge::provider_for_host(&host).unwrap_or("git");
    SourceRepoSlug::with_provider_and_host(normalized.owner, normalized.repo, provider, Some(host))
        .map_err(|e| unsupported(e.to_string()))
}

/// The identity of a folder with no usable remote: `local/<folder>` on the
/// `local` host, the key remote-less attachments are looked up by.
fn local_slug(path: &Path) -> Result<SourceRepoSlug, AddRepoError> {
    let folder =
        path.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| AddRepoError::InvalidInput {
                reason: format!("{} has no folder name", path.display()),
            })?;
    SourceRepoSlug::with_provider_and_host("local", folder, "local", Some("local")).map_err(|e| {
        AddRepoError::InvalidInput {
            reason: format!("the folder name {folder:?} can't name a repo: {e}"),
        }
    })
}

/// An absolute path to an existing folder, canonicalized (symlinks resolved,
/// no trailing slash) so one folder is always stored as one path.
fn existing_folder(path: &str) -> Result<PathBuf, AddRepoError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(AddRepoError::InvalidInput {
            reason: "the folder path is empty".to_string(),
        });
    }
    let folder = PathBuf::from(trimmed);
    if !folder.is_absolute() {
        return Err(AddRepoError::InvalidInput {
            reason: format!("{trimmed} is not an absolute path"),
        });
    }
    if !folder.is_dir() {
        return Err(AddRepoError::NotADirectory {
            path: trimmed.to_string(),
        });
    }
    let canonical = folder
        .canonicalize()
        .map_err(|e| failure(format!("can't resolve {trimmed}: {e}")))?;
    if canonical.to_str().is_none() {
        return Err(AddRepoError::InvalidInput {
            reason: "The canonical folder path must be valid UTF-8".into(),
        });
    }
    Ok(canonical)
}

/// Whether `folder` (canonical) is a working-tree root: `Ok(true)` for a root,
/// `Ok(false)` for a folder in no repository, and a refusal naming the root
/// for a folder inside a working tree (or its `.git` directory). A repository
/// rooted at the user's home directory (a dotfile-managed `$HOME`) does not
/// enclose the folders under it.
fn is_repo_root(folder: &Path) -> Result<bool, AddRepoError> {
    let home = dirs::home_dir().and_then(|h| h.canonicalize().ok());
    is_repo_root_under(folder, home.as_deref())
}

fn is_repo_root_under(folder: &Path, home: Option<&Path>) -> Result<bool, AddRepoError> {
    match crate::commands::git::working_tree_root_of(folder) {
        Some(root) if root == folder => Ok(crate::commands::git::is_git_working_tree(folder)),
        Some(root) if Some(root.as_path()) == home && !folder.starts_with(root.join(".git")) => {
            Ok(false)
        }
        Some(root) => Err(AddRepoError::NotRepoRoot {
            path: folder.display().to_string(),
            root: root.display().to_string(),
        }),
        None => Ok(false),
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

/// A folder to bind: its canonical path and the remote that names its repo.
#[derive(Debug, Clone)]
struct PickedClone {
    path: String,
    remote: Option<(String, String)>,
}

/// The repo a folder being added names. In a fork workflow `upstream` is the
/// canonical repo, so it wins over `origin`; a folder with neither, or whose
/// git config can't be read, is `local/<folder>`.
fn inspect_folder(path: &str) -> Result<(SourceRepoSlug, PickedClone), AddRepoError> {
    let folder = existing_folder(path)?;
    let remotes = if is_repo_root(&folder)? {
        crate::commands::git::remotes_at(&folder).unwrap_or_else(|e| {
            log::warn!("reading the remotes of {}: {e}", folder.display());
            Vec::new()
        })
    } else {
        Vec::new()
    };
    let named = |name: &str| remotes.iter().find(|(n, _)| n == name).cloned();
    let candidates: Vec<(String, String)> = ["upstream", "origin"]
        .into_iter()
        .filter_map(named)
        .collect();
    let mut first_refusal = None;
    for (name, url) in &candidates {
        match slug_from_remote(url) {
            Ok(slug) => {
                let clone = PickedClone {
                    path: display(&folder),
                    remote: Some((name.clone(), url.clone())),
                };
                return Ok((slug, clone));
            }
            Err(e) => {
                first_refusal.get_or_insert(e);
            }
        }
    }
    if let Some(refusal) = first_refusal {
        return Err(refusal);
    }
    let clone = PickedClone {
        path: display(&folder),
        remote: None,
    };
    Ok((local_slug(&folder)?, clone))
}

/// Check that `path` is the root of a working tree with a remote naming
/// `slug`. Remotes are tried `origin`, then `upstream`, then the rest by name,
/// so a fork whose `upstream` is the team's repo binds through `upstream`.
fn verify_clone_of(slug: &SourceRepoSlug, path: &str) -> Result<PickedClone, AddRepoError> {
    let folder = existing_folder(path)?;
    if !is_repo_root(&folder)? {
        return Err(AddRepoError::NotAGitRepository {
            path: display(&folder),
        });
    }
    let mut remotes = crate::commands::git::remotes_at(&folder).unwrap_or_default();
    let rank = |name: &str| match name {
        "origin" => 0,
        "upstream" => 1,
        _ => 2,
    };
    remotes.sort_by(|(a, _), (b, _)| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    if remotes.is_empty() {
        return Err(AddRepoError::NoOrigin {
            path: display(&folder),
        });
    }
    let key = slug.remote_key();
    let parsed: Vec<_> = remotes
        .iter()
        .map(|(name, url)| (name, url, slug_from_remote(url).map(|s| s.remote_key())))
        .collect();
    if let Some((name, url, _)) = parsed
        .iter()
        .find(|(_, _, k)| k.as_deref().ok() == Some(key.as_str()))
    {
        return Ok(PickedClone {
            path: display(&folder),
            remote: Some(((*name).clone(), (*url).clone())),
        });
    }
    let actual = parsed
        .iter()
        .find_map(|(_, _, k)| k.clone().ok())
        .unwrap_or_else(|| remotes[0].1.clone());
    Err(AddRepoError::OriginMismatch {
        expected: key,
        actual,
    })
}

/// The source repos of a non-archived team in a local org.
pub(crate) fn team_source_repos(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<Vec<SourceRepoSlug>, AddRepoError> {
    let unknown = || AddRepoError::UnknownTeam {
        org: org.to_string(),
        team: team.to_string(),
    };
    crate::context_repo::org_slug::require_valid_org_slug(org)
        .map_err(|reason| AddRepoError::InvalidInput { reason })?;
    let org_root = crate::context_repo::path_to_org_root(home, principal, org).map_err(failure)?;
    if !org_root.is_dir() {
        return Err(unknown());
    }
    let loaded = crate::domain::load_org(org.to_string(), &org_root, None);
    let Some(composed) = loaded.org else {
        return Err(failure(format!("{org}'s registry failed to load")));
    };
    composed
        .teams
        .into_iter()
        .find(|t| t.name == team && !t.archived)
        .map(|t| t.source_repos)
        .ok_or_else(unknown)
}

/// The tracked repo `repo` names on a team: `owner/repo` or `host/owner/repo`.
pub(crate) fn resolve_tracked_repo(
    tracked: &[SourceRepoSlug],
    repo: &str,
    team: &str,
) -> Result<SourceRepoSlug, AddRepoError> {
    SourceRepoSlug::resolve_against(repo, tracked)
        .filter(|slug| tracked.iter().any(|t| t.remote_key() == slug.remote_key()))
        .ok_or_else(|| AddRepoError::NotTracked {
            repo: repo.trim().to_string(),
            team: team.to_string(),
        })
}

/// Whether the user has a clone of the repo keyed `key` bound at a folder that
/// still exists.
fn has_live_binding(home: &Path, principal: &Principal, key: &str) -> Result<bool, AddRepoError> {
    let path = crate::context_repo::path_to_user_attachments_file(home, principal);
    let data = crate::context_repo::attachments_io::read_attachments(&path)
        .map_err(|e| failure(e.to_string()))?;
    Ok(super::attachments::group_attachments_by_remote(&data)
        .get(key)
        .is_some_and(|a| Path::new(&a.repo_path).is_dir()))
}

/// Tell every view the user's attachments changed. Best-effort: the record is
/// already on disk.
pub(crate) fn emit_source_repo_attachments_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    principal: &Principal,
) {
    use tauri::Emitter;
    if let Err(e) = app.emit(
        SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT,
        serde_json::json!({"principal": principal.as_str()}),
    ) {
        log::warn!("{SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT} emit failed: {e}");
    }
}

/// What the add and bind operations need from the running app.
pub(crate) struct AttachmentEnv<'a, R: tauri::Runtime> {
    pub app: &'a tauri::AppHandle<R>,
    pub coord: &'a WaveCoordinator,
    pub active_team: &'a ActiveTeamState,
    pub home: &'a Path,
    pub user_id: u64,
}

fn validate_picked_clone(clone: &PickedClone) -> Result<(), AddRepoError> {
    crate::domain::validation::validate_non_empty_no_control(&clone.path, "repo_path").map_err(
        |e| AddRepoError::InvalidInput {
            reason: e.to_string(),
        },
    )?;
    if let Some((_, url)) = &clone.remote {
        crate::domain::validation::validate_non_empty_no_control(url, "remote_url").map_err(
            |e| AddRepoError::InvalidInput {
                reason: e.to_string(),
            },
        )?;
    }
    Ok(())
}

impl<R: tauri::Runtime> AttachmentEnv<'_, R> {
    /// Bind `clone` as the one clone of `slug`: every other record under the
    /// repo's key is dropped, so rebinding never leaves a stale duplicate.
    async fn save_binding(
        &self,
        slug: &SourceRepoSlug,
        clone: &PickedClone,
    ) -> Result<(), AddRepoError> {
        validate_picked_clone(clone)?;
        let attachment = Attachment {
            repo_path: clone.path.clone(),
            remote_url: clone.remote.as_ref().map(|(_, url)| url.clone()),
            attached_at: None,
            tracking_prompt_dismissed_at: None,
            credential_ref: None,
            managed: false,
        };
        let (home, user_id, key) = (self.home.to_path_buf(), self.user_id, slug.remote_key());
        let upserted = blocking("source_repo_add.save_binding", move || {
            upsert_attachment_record(&home, user_id, attachment, Some(&key), true).map_err(failure)
        })
        .await?;
        reconcile_after_upsert(
            self.home,
            self.user_id,
            &upserted,
            self.coord,
            self.active_team,
        )
        .await;
        emit_source_repo_attachments_changed(self.app, &Principal::from(self.user_id));
        Ok(())
    }
}

// ---- operations -------------------------------------------------------------

/// Validated source identity and planned binding.
struct AddPlan {
    slug: SourceRepoSlug,
    clone: Option<PickedClone>,
    /// The team already tracks the repo; only its clone is bound.
    bind_only: bool,
}

/// Add a repo to `team` in `org`. A URL is tracked on the team. A local folder
/// is tracked under the repo its `upstream` (else `origin`) remote names, or
/// `local/<folder>` with neither, and bound as that repo's local clone. A repo
/// the team already tracks is refused — unless it was added by folder and no
/// live clone is bound to it yet, in which case the folder is just bound. The
/// selected clone snapshot is validated before anything is written. The registry change emits `org-registry-changed`; a bound
/// clone emits [`SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT`].
pub(crate) async fn add_source_repo_to_team<R>(
    env: &AttachmentEnv<'_, R>,
    org: &str,
    team: &str,
    input: AddRepoInput,
) -> Result<TrackedSourceRepo, AddRepoError>
where
    R: tauri::Runtime,
{
    let watcher = env.app.state::<super::watch::WatcherState>();
    let _activation = watcher.lock_team_activation().await;
    env.app
        .state::<super::local_profile::LocalProfileState>()
        .check_expected(Some(&env.user_id.to_string()))
        .map_err(failure)?;
    let org = org.trim().to_string();
    let team = team.trim().to_string();
    if org.is_empty() || team.is_empty() {
        return Err(AddRepoError::InvalidInput {
            reason: "org and team must be non-empty".to_string(),
        });
    }

    let plan = {
        let (home, principal) = (env.home.to_path_buf(), Principal::from(env.user_id));
        let (org, team) = (org.clone(), team.clone());
        blocking("source_repo_add.plan", move || {
            plan_add(&home, &principal, &org, &team, input)
        })
        .await?
    };

    if let Some(clone) = &plan.clone {
        validate_picked_clone(clone)?;
    }
    if !plan.bind_only {
        let edit = RegistryEdit::AddSourceRepoToTeam {
            team_name: team.clone(),
            owner: plan.slug.owner.clone(),
            repo: plan.slug.repo.clone(),
            provider: Some(plan.slug.provider.clone()),
            host: Some(plan.slug.canonical_host().to_string()),
        };
        super::registry_local::commit_registry_edit(
            env.app,
            env.home,
            &Principal::from(env.user_id),
            &org,
            edit,
        )
        .await
        .map_err(failure)?;
    }

    if let Some(clone) = &plan.clone {
        env.save_binding(&plan.slug, clone).await.map_err(|e| {
            failure(format!(
                "tracked {} on team {team}, but binding its folder failed: {e}",
                plan.slug
            ))
        })?;
    }
    Ok(TrackedSourceRepo::new(&plan.slug, plan.clone.as_ref()))
}

/// The repo an add names, and whether it only needs its clone bound.
fn plan_add(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
    input: AddRepoInput,
) -> Result<AddPlan, AddRepoError> {
    let tracked = team_source_repos(home, principal, org, team)?;
    let (slug, clone) = match input {
        AddRepoInput::Url { url } => (slug_from_url(&url)?, None),
        AddRepoInput::LocalPath { path } => {
            let (slug, clone) = inspect_folder(&path)?;
            (slug, Some(clone))
        }
    };
    let already_tracked = tracked.iter().any(|t| t.remote_key() == slug.remote_key());
    if already_tracked
        && (clone.is_none() || has_live_binding(home, principal, &slug.remote_key())?)
    {
        return Err(AddRepoError::AlreadyTracked {
            owner: slug.owner,
            repo: slug.repo,
            team: team.to_string(),
        });
    }
    Ok(AddPlan {
        slug,
        clone,
        bind_only: already_tracked,
    })
}

/// Bind the clone at `path` to `slug`, replacing any clone bound before. The
/// folder must be a working-tree root with a remote naming that repo; the
/// record stores that remote's URL. Emits
/// [`SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT`].
pub(crate) async fn attach_source_repo_clone<R>(
    env: &AttachmentEnv<'_, R>,
    slug: &SourceRepoSlug,
    path: &str,
) -> Result<TrackedSourceRepo, AddRepoError>
where
    R: tauri::Runtime,
{
    let watcher = env.app.state::<super::watch::WatcherState>();
    let _activation = watcher.lock_team_activation().await;
    env.app
        .state::<super::local_profile::LocalProfileState>()
        .check_expected(Some(&env.user_id.to_string()))
        .map_err(failure)?;
    attach_source_repo_clone_locked(env, slug, path).await
}

/// Caller owns the profile/team activation lock through lookup and binding.
pub(crate) async fn attach_source_repo_clone_locked<R: tauri::Runtime>(
    env: &AttachmentEnv<'_, R>,
    slug: &SourceRepoSlug,
    path: &str,
) -> Result<TrackedSourceRepo, AddRepoError> {
    let clone = {
        let (slug, path) = (slug.clone(), path.to_string());
        blocking("source_repo_add.verify_clone", move || {
            verify_clone_of(&slug, &path)
        })
        .await?
    };
    env.save_binding(slug, &clone).await?;
    Ok(TrackedSourceRepo::new(slug, Some(&clone)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::context_repo::{path_to_org_root, path_to_user_attachments_file};
    use std::sync::{Arc, Mutex};
    use tauri::Listener;

    const ORG: &str = "acme/context";
    const UID: u64 = 7;

    struct Fixture {
        home: tempfile::TempDir,
        app: tauri::App<tauri::test::MockRuntime>,
        coord: WaveCoordinator,
        active: ActiveTeamState,
        events: Arc<Mutex<Vec<String>>>,
    }

    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let org_root = path_to_org_root(home.path(), &Principal::from(UID), ORG).unwrap();
            std::fs::create_dir_all(&org_root).unwrap();
            std::fs::write(
                org_root.join("org.yml"),
                format!("slug: {ORG}\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
            )
            .unwrap();
            std::fs::write(
                org_root.join("people.yml"),
                "people:\n  - github_user_id: 7\n    github_username: dev\n",
            )
            .unwrap();
            std::fs::write(
                org_root.join("teams.yml"),
                "teams:\n  - name: backend\n    source_repos:\n      - owner: acme\n        repo: api\n  - name: legacy\n    archived: true\n",
            )
            .unwrap();
            git2::Repository::init(&org_root).unwrap();

            let app = mock_app();
            let events = Arc::new(Mutex::new(Vec::new()));
            for name in [
                SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT,
                super::super::registry_local::ORG_REGISTRY_CHANGED_EVENT,
            ] {
                let sink = events.clone();
                app.listen(name, move |_| sink.lock().unwrap().push(name.to_string()));
            }
            Self {
                home,
                app,
                coord: WaveCoordinator::default(),
                active: ActiveTeamState::default(),
                events,
            }
        }

        fn env(&self) -> AttachmentEnv<'_, tauri::test::MockRuntime> {
            AttachmentEnv {
                app: self.app.handle(),
                coord: &self.coord,
                active_team: &self.active,
                home: self.home.path(),
                user_id: UID,
            }
        }

        async fn add(&self, input: AddRepoInput) -> Result<TrackedSourceRepo, AddRepoError> {
            add_source_repo_to_team(&self.env(), ORG, "backend", input).await
        }

        async fn attach(&self, repo: &str, path: &Path) -> Result<TrackedSourceRepo, AddRepoError> {
            let slug = SourceRepoSlug::parse_owner_repo(repo).unwrap();
            attach_source_repo_clone(&self.env(), &slug, path.to_str().unwrap()).await
        }

        fn tracked(&self) -> Vec<String> {
            team_source_repos(self.home.path(), &Principal::from(UID), ORG, "backend")
                .unwrap()
                .iter()
                .map(SourceRepoSlug::remote_key)
                .collect()
        }

        fn attachments(&self) -> Vec<Attachment> {
            let path = path_to_user_attachments_file(self.home.path(), &Principal::from(UID));
            crate::context_repo::attachments_io::read_attachments(&path)
                .unwrap()
                .attachments
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }

        /// A git working tree under the home dir, with `origin` when given.
        fn clone_dir(&self, name: &str, origin: Option<&str>) -> PathBuf {
            let remotes: Vec<(&str, &str)> =
                origin.map(|url| ("origin", url)).into_iter().collect();
            self.clone_with_remotes(name, &remotes)
        }

        /// A git working tree under the home dir with these remotes.
        fn clone_with_remotes(&self, name: &str, remotes: &[(&str, &str)]) -> PathBuf {
            let dir = self.home.path().join("code").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let repo = git2::Repository::init(&dir).unwrap();
            for (remote, url) in remotes {
                repo.remote(remote, url).unwrap();
            }
            dir.canonicalize().unwrap()
        }
    }

    /// A mock app holding the auth state the registry commit reads.
    pub(crate) fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        let home = tempfile::tempdir().unwrap();
        let profile = super::super::local_profile::LocalProfileState::for_test(
            home.path(),
            UID,
            super::super::local_profile::MachineOwner::Local {
                id: uuid::Uuid::new_v4().to_string(),
            },
        );
        tauri::test::mock_builder()
            .manage(profile)
            .manage(super::super::watch::WatcherState::default())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app")
    }

    #[tokio::test]
    async fn a_url_add_tracks_the_repo_and_binds_no_clone() {
        let fx = Fixture::new();
        let added = fx
            .add(AddRepoInput::Url {
                url: "https://gitlab.com/Acme/Web.git".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            (
                added.owner.as_str(),
                added.repo.as_str(),
                added.provider.as_str()
            ),
            ("Acme", "Web", "gitlab")
        );
        assert_eq!(added.remote_key, "gitlab.com/acme/web");
        assert_eq!(added.local_path, None);
        assert_eq!(fx.tracked(), ["github.com/acme/api", "gitlab.com/acme/web"]);
        assert!(fx.attachments().is_empty());
        assert_eq!(fx.events(), ["org-registry-changed"]);
    }

    #[tokio::test]
    async fn owner_repo_shorthand_is_github() {
        let fx = Fixture::new();
        let added = fx
            .add(AddRepoInput::Url {
                url: " acme/worker ".into(),
            })
            .await
            .unwrap();
        assert_eq!(added.remote_key, "github.com/acme/worker");
        let host_form = slug_from_url("github.com/acme/worker").unwrap();
        assert_eq!(host_form.remote_key(), "github.com/acme/worker");
    }

    // macOS rejects invalid UTF-8 filenames before the canonical-path guard is reached.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn non_utf8_canonical_target_is_refused_without_writes() {
        use std::os::unix::ffi::OsStringExt;
        let fx = Fixture::new();
        let target = fx
            .home
            .path()
            .join(std::ffi::OsString::from_vec(b"target-\xff".to_vec()));
        std::fs::create_dir(&target).unwrap();
        let alias = fx.home.path().join("readable-alias");
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        let error = fx
            .add(AddRepoInput::LocalPath {
                path: alias.to_str().unwrap().into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, AddRepoError::InvalidInput { .. }));
        assert_eq!(fx.tracked(), ["github.com/acme/api"]);
        assert!(fx.attachments().is_empty());
        assert!(fx.events().is_empty());
    }

    #[tokio::test]
    async fn control_character_path_is_refused_before_registry_or_binding_write() {
        let fx = Fixture::new();
        let dir = fx.clone_dir("unsafe\nfolder/web", Some("git@github.com:acme/web.git"));
        let error = fx
            .add(AddRepoInput::LocalPath {
                path: dir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, AddRepoError::InvalidInput { .. }));
        assert_eq!(fx.tracked(), ["github.com/acme/api"]);
        assert!(fx.attachments().is_empty());
        assert!(fx.events().is_empty());
    }

    #[tokio::test]
    async fn self_hosted_checkout_tracks_attaches_and_resolves_without_provider_auth() {
        let fx = Fixture::new();
        let dir = fx.clone_dir("enterprise", Some("git@git.corp.example:Acme/Web.git"));
        let added = fx
            .add(AddRepoInput::LocalPath {
                path: dir.to_str().unwrap().into(),
            })
            .await
            .unwrap();
        assert_eq!(added.remote_key, "git.corp.example/acme/web");
        assert_eq!(added.provider, "git");
        assert_eq!(added.local_path.as_deref(), dir.to_str());
        assert!(fx
            .tracked()
            .contains(&"git.corp.example/acme/web".to_string()));
        let grouped =
            super::super::attachments::group_attachments_by_remote(&crate::domain::Attachments {
                attachments: fx.attachments(),
            });
        assert!(grouped.contains_key(&added.remote_key));
        let slug = crate::operations::git_remote::derive_repository_identity(&dir).unwrap();
        assert_eq!(slug.remote_key(), added.remote_key);
        assert_eq!(slug.provider, "git");
        assert_eq!(
            serde_json::from_value::<SourceRepoSlug>(serde_json::to_value(&slug).unwrap()).unwrap(),
            slug
        );
        assert_eq!(
            slug_from_url("https://git.corp.example/Acme/Web.git").unwrap(),
            slug
        );
    }

    #[tokio::test]
    async fn a_local_path_add_reads_origin_and_binds_the_clone() {
        let fx = Fixture::new();
        let dir = fx.clone_dir("web", Some("git@github.com:acme/web.git"));
        let added = fx
            .add(AddRepoInput::LocalPath {
                path: dir.to_str().unwrap().into(),
            })
            .await
            .unwrap();
        assert_eq!(added.remote_key, "github.com/acme/web");
        assert_eq!(added.local_path.as_deref(), dir.to_str());
        assert_eq!(
            added.remote_url.as_deref(),
            Some("git@github.com:acme/web.git")
        );
        assert_eq!(fx.tracked(), ["github.com/acme/api", "github.com/acme/web"]);
        let attachments = fx.attachments();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].repo_path, dir.to_str().unwrap());
        assert_eq!(
            attachments[0].remote_url.as_deref(),
            Some("git@github.com:acme/web.git")
        );
        assert_eq!(
            fx.events(),
            [
                "org-registry-changed",
                SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT
            ]
        );
    }

    #[tokio::test]
    async fn a_folder_without_origin_is_tracked_as_local_folder_name() {
        let fx = Fixture::new();
        let git_no_origin = fx.clone_dir("scratch", None);
        let plain = fx.home.path().join("notes");
        std::fs::create_dir_all(&plain).unwrap();

        for (dir, key) in [
            (git_no_origin, "local/local/scratch"),
            (plain, "local/local/notes"),
        ] {
            let added = fx
                .add(AddRepoInput::LocalPath {
                    path: dir.to_str().unwrap().into(),
                })
                .await
                .unwrap();
            assert_eq!(added.remote_key, key);
            assert_eq!(added.owner, "local");
            assert_eq!(added.remote_url, None);
        }
        let attachments = fx.attachments();
        assert_eq!(attachments.len(), 2);
        assert!(attachments.iter().all(|a| a.remote_url.is_none()));
        assert!(fx.tracked().contains(&"local/local/notes".to_string()));
    }

    #[tokio::test]
    async fn a_repo_the_team_already_tracks_is_refused_and_nothing_changes() {
        let fx = Fixture::new();
        let err = fx
            .add(AddRepoInput::Url {
                url: "https://github.com/acme/api".into(),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, AddRepoError::AlreadyTracked { ref team, .. } if team == "backend"),
            "{err:?}"
        );
        assert_eq!(fx.tracked(), ["github.com/acme/api"]);
        assert!(fx.attachments().is_empty());
        assert!(fx.events().is_empty());
    }

    #[tokio::test]
    async fn adding_the_folder_of_a_tracked_repo_with_no_clone_just_binds_it() {
        let fx = Fixture::new();
        let dir = fx.clone_dir("api", Some("https://github.com/ACME/API.git"));
        let input = || AddRepoInput::LocalPath {
            path: dir.to_str().unwrap().into(),
        };
        let added = fx.add(input()).await.unwrap();
        assert_eq!(added.local_path.as_deref(), dir.to_str());
        assert_eq!(fx.tracked(), ["github.com/acme/api"]);
        assert_eq!(fx.attachments().len(), 1);
        assert_eq!(fx.events(), [SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT]);

        // Now a live clone is bound, so a second add is a duplicate.
        assert!(matches!(
            fx.add(input()).await.unwrap_err(),
            AddRepoError::AlreadyTracked { .. }
        ));
    }

    #[tokio::test]
    async fn one_clone_is_stored_once_whatever_the_spelling_of_its_path() {
        let fx = Fixture::new();
        let dir = fx.clone_dir("api", Some("https://github.com/acme/api.git"));
        let link = fx.home.path().join("api-link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();

        for spelling in [
            format!("{}/", dir.display()),
            link.display().to_string(),
            dir.display().to_string(),
        ] {
            let bound = fx.attach("acme/api", Path::new(&spelling)).await.unwrap();
            assert_eq!(bound.local_path.as_deref(), dir.to_str());
        }
        let attachments = fx.attachments();
        assert_eq!(attachments.len(), 1, "{attachments:?}");
        assert_eq!(attachments[0].repo_path, dir.to_str().unwrap());
    }

    #[tokio::test]
    async fn rebinding_a_repo_drops_its_old_clone_record() {
        let fx = Fixture::new();
        let first = fx.clone_dir("api", Some("https://github.com/acme/api.git"));
        let second = fx.clone_dir("api-2", Some("git@github.com:ACME/api.git"));
        fx.attach("acme/api", &first).await.unwrap();
        fx.attach("acme/api", &second).await.unwrap();
        let attachments = fx.attachments();
        assert_eq!(attachments.len(), 1, "{attachments:?}");
        assert_eq!(attachments[0].repo_path, second.to_str().unwrap());
    }

    #[tokio::test]
    async fn a_fork_binds_through_the_remote_that_names_the_team_repo() {
        let fx = Fixture::new();
        let dir = fx.clone_with_remotes(
            "api-fork",
            &[
                ("origin", "https://github.com/me/api.git"),
                ("upstream", "https://github.com/acme/api.git"),
            ],
        );
        let bound = fx.attach("acme/api", &dir).await.unwrap();
        assert_eq!(bound.remote_name.as_deref(), Some("upstream"));
        assert_eq!(
            bound.remote_url.as_deref(),
            Some("https://github.com/acme/api.git")
        );
        // The stored URL is the matching remote's, so the record keys to the repo.
        let by_remote =
            super::super::attachments::group_attachments_by_remote(&crate::domain::Attachments {
                attachments: fx.attachments(),
            });
        assert_eq!(
            by_remote["github.com/acme/api"].repo_path,
            dir.to_str().unwrap()
        );
    }

    #[tokio::test]
    async fn adding_a_fork_by_folder_tracks_its_upstream() {
        let fx = Fixture::new();
        let dir = fx.clone_with_remotes(
            "web-fork",
            &[
                ("origin", "https://github.com/me/web.git"),
                ("upstream", "https://github.com/acme/web.git"),
            ],
        );
        let added = fx
            .add(AddRepoInput::LocalPath {
                path: dir.to_str().unwrap().into(),
            })
            .await
            .unwrap();
        assert_eq!(added.remote_key, "github.com/acme/web");
        assert_eq!(added.remote_name.as_deref(), Some("upstream"));
        assert!(fx.tracked().contains(&"github.com/acme/web".to_string()));
    }

    #[tokio::test]
    async fn a_folder_inside_a_repo_is_refused_with_the_root_to_pick() {
        let fx = Fixture::new();
        let dir = fx.clone_dir("api", Some("https://github.com/acme/api.git"));
        let sub = dir.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        for picked in [sub, dir.join(".git")] {
            let expected = AddRepoError::NotRepoRoot {
                path: picked.display().to_string(),
                root: dir.display().to_string(),
            };
            assert_eq!(fx.attach("acme/api", &picked).await.unwrap_err(), expected);
            let added = fx
                .add(AddRepoInput::LocalPath {
                    path: picked.display().to_string(),
                })
                .await
                .unwrap_err();
            assert_eq!(added, expected);
        }
        assert!(fx.events().is_empty());
    }

    #[test]
    fn a_git_tracked_home_does_not_enclose_the_folders_under_it() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        git2::Repository::init(&home).unwrap();
        let folder = home.join("code").join("notes");
        std::fs::create_dir_all(&folder).unwrap();

        // Under a dotfile-managed home, a plain folder is in no repository.
        assert_eq!(is_repo_root_under(&folder, Some(&home)), Ok(false));
        assert_eq!(
            local_slug(&folder).unwrap().remote_key(),
            "local/local/notes"
        );
        // The home repo's own `.git` is still refused.
        assert!(matches!(
            is_repo_root_under(&home.join(".git"), Some(&home)),
            Err(AddRepoError::NotRepoRoot { .. })
        ));
        // Any other enclosing repo still refuses, naming its root.
        assert_eq!(
            is_repo_root_under(&folder, None),
            Err(AddRepoError::NotRepoRoot {
                path: folder.display().to_string(),
                root: home.display().to_string(),
            })
        );
    }

    #[tokio::test]
    async fn an_unreadable_git_config_adds_the_folder_by_name() {
        let fx = Fixture::new();
        let dir = fx.clone_dir("broken", Some("https://github.com/acme/broken.git"));
        std::fs::write(
            dir.join(".git").join("config"),
            "[remote \"origin\"\n url = =",
        )
        .unwrap();
        let added = fx
            .add(AddRepoInput::LocalPath {
                path: dir.to_str().unwrap().into(),
            })
            .await
            .unwrap();
        assert_eq!(added.remote_key, "local/local/broken");
    }

    #[tokio::test]
    async fn bad_inputs_are_typed_refusals() {
        let fx = Fixture::new();
        let file = fx.home.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();
        let cases = [
            (
                AddRepoInput::Url {
                    url: "https://bad_host/a/b".into(),
                },
                "unsupportedRemote",
            ),
            (AddRepoInput::Url { url: "  ".into() }, "invalidInput"),
            (
                AddRepoInput::LocalPath {
                    path: "relative/dir".into(),
                },
                "invalidInput",
            ),
            (
                AddRepoInput::LocalPath {
                    path: file.to_str().unwrap().into(),
                },
                "notADirectory",
            ),
        ];
        for (input, kind) in cases {
            let err = fx.add(input).await.unwrap_err();
            assert_eq!(serde_json::to_value(&err).unwrap()["kind"], kind, "{err:?}");
        }
        let archived = add_source_repo_to_team(
            &fx.env(),
            ORG,
            "legacy",
            AddRepoInput::Url {
                url: "acme/x".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(archived, AddRepoError::UnknownTeam { .. }));
        assert!(fx.events().is_empty());
    }

    #[tokio::test]
    async fn attaching_a_clone_with_a_matching_origin_binds_it() {
        let fx = Fixture::new();
        let dir = fx.clone_dir("api", Some("https://github.com/Acme/Api.git"));
        let bound = fx.attach("acme/api", &dir).await.unwrap();
        assert_eq!(bound.local_path.as_deref(), dir.to_str());
        assert_eq!(fx.attachments().len(), 1);
        assert_eq!(fx.events(), [SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT]);

        // Rebinding to another clone of the same repo replaces the lookup.
        let other = fx.clone_dir("api-2", Some("git@github.com:acme/api.git"));
        fx.attach("acme/api", &other).await.unwrap();
        let by_remote =
            super::super::attachments::group_attachments_by_remote(&crate::domain::Attachments {
                attachments: fx.attachments(),
            });
        assert_eq!(
            by_remote["github.com/acme/api"].repo_path,
            other.to_str().unwrap()
        );
    }

    #[tokio::test]
    async fn attaching_a_clone_of_another_repo_is_refused() {
        let fx = Fixture::new();
        let dir = fx.clone_dir("fork", Some("https://github.com/someone/api.git"));
        let err = fx.attach("acme/api", &dir).await.unwrap_err();
        assert_eq!(
            err,
            AddRepoError::OriginMismatch {
                expected: "github.com/acme/api".into(),
                actual: "github.com/someone/api".into(),
            }
        );
        let no_origin = fx.clone_dir("bare-ish", None);
        assert!(matches!(
            fx.attach("acme/api", &no_origin).await.unwrap_err(),
            AddRepoError::NoOrigin { .. }
        ));
        let plain = fx.home.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(matches!(
            fx.attach("acme/api", &plain).await.unwrap_err(),
            AddRepoError::NotAGitRepository { .. }
        ));
        assert!(fx.attachments().is_empty());
        assert!(fx.events().is_empty());
    }

    #[test]
    fn a_failure_serializes_its_kind_fields_and_message() {
        let wire = serde_json::to_value(AddRepoFailure::from(AddRepoError::AlreadyTracked {
            owner: "acme".into(),
            repo: "api".into(),
            team: "backend".into(),
        }))
        .unwrap();
        assert_eq!(wire["kind"], "alreadyTracked");
        assert_eq!(wire["owner"], "acme");
        assert_eq!(
            wire["message"],
            "acme/api is already tracked on team backend"
        );
    }

    #[test]
    fn a_tracked_repo_resolves_by_owner_repo_or_remote_key_only() {
        let tracked = vec![
            SourceRepoSlug::parse_owner_repo("acme/api").unwrap(),
            slug_from_url("https://gitlab.com/acme/web").unwrap(),
        ];
        assert_eq!(
            resolve_tracked_repo(&tracked, "ACME/api", "t")
                .unwrap()
                .remote_key(),
            "github.com/acme/api"
        );
        assert_eq!(
            resolve_tracked_repo(&tracked, "gitlab.com/acme/web", "t")
                .unwrap()
                .provider,
            "gitlab"
        );
        assert!(matches!(
            resolve_tracked_repo(&tracked, "acme/other", "t"),
            Err(AddRepoError::NotTracked { .. })
        ));
    }
}
