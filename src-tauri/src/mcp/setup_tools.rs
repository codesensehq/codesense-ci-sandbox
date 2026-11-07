// SPDX-License-Identifier: MIT
//! The setup MCP tools: switching the app's active team, describing what an
//! agent is looking at when it sets CodeSense up, and managing a team's source
//! repos (add, bind a local clone, remove).
//!
//! The `#[tool]` methods in [`super::handler`] resolve the selected local profile and
//! hand the bodies here the app's real team-activation path; the bodies own the
//! parsing, the registry reads and the wire shapes. Every read goes through the
//! existing resolvers (`discover_local_orgs_impl`, `load_org`, the attachments
//! store) — this module keeps no state of its own.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};

use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::route_to_context::{resolve_local, ContextResolution};
use crate::commands::source_repo_add::{
    add_source_repo_to_team, resolve_tracked_repo, team_source_repos, AddRepoError, AddRepoInput,
    AttachmentEnv, TrackedSourceRepo,
};
use crate::context_repo::{path_to_org_root, path_to_team_content_dir};
use crate::domain::{Attachment, Principal, SourceRepoSlug, Team};

pub(crate) use super::tool_envelope::ToolEnvelope;

/// What a body returns: a typed envelope, or a hard failure that surfaces as an
/// MCP error.
pub(crate) type Reply<T> = Result<ToolEnvelope<T>, String>;

/// The event that asks the UI to show a team an agent switched to. The backend
/// is already switched when it fires; the UI applies it the way a team click
/// does, so its own views follow.
pub const ACTIVE_TEAM_SWITCH_EVENT: &str = "mcp:set-active-team";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetActiveTeamParams {
    /// The context-repo org slug (`owner/repo`), as `describe_setup` or
    /// `list_orgs_teams` reports it.
    pub org: String,
    /// The team to make active within `org`.
    pub team: String,
}

/// The `{ org, team }` a switch landed on, also the switch event's payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTeamSwitched {
    pub org_slug: String,
    pub team: String,
}

/// Everything the app's team activation needs, resolved from the registry.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TeamActivation {
    pub codesense_home: PathBuf,
    pub user_id: u64,
    pub org_slug: String,
    pub team: String,
    pub source_repos: Vec<SourceRepoSlug>,
}

/// Switch the active team: confirm `(org, team)` is a non-archived team in a
/// local org, then run `activate` — the app's team-activation path — with the
/// team's source repos. A team that is listed but has no local content yet is
/// still switchable, exactly as clicking it in the team switcher is.
pub(crate) async fn set_active_team<F, Fut>(
    codesense_home: PathBuf,
    user_id: u64,
    params: SetActiveTeamParams,
    activate: F,
) -> Reply<ActiveTeamSwitched>
where
    F: FnOnce(TeamActivation) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let org = params.org.trim().to_string();
    let team = params.team.trim().to_string();
    if org.is_empty() || team.is_empty() {
        return Ok(ToolEnvelope::Validation {
            message: "org and team must be non-empty".to_string(),
        });
    }
    let activation = {
        let (org, team) = (org.clone(), team.clone());
        crate::commands::await_blocking("mcp.set_active_team", move || {
            Ok(team_activation(codesense_home, user_id, org, team))
        })
        .await?
    };
    let Some(activation) = activation else {
        return Ok(ToolEnvelope::UnauthorizedTeam);
    };
    activate(activation).await?;
    Ok(ToolEnvelope::Ok(ActiveTeamSwitched {
        org_slug: org,
        team,
    }))
}

/// The activation for `(org, team)`, or `None` when it is not a non-archived
/// team in a local org. Membership is decided by the same resolver every
/// any-team tool uses; the source repos come from the team's registry entry.
fn team_activation(
    codesense_home: PathBuf,
    user_id: u64,
    org: String,
    team: String,
) -> Option<TeamActivation> {
    match resolve_local(codesense_home.clone(), user_id, org.clone(), team.clone()) {
        ContextResolution::Ok(_) | ContextResolution::NotSynced => {}
        ContextResolution::Unauthorized => return None,
    }
    let org_root = path_to_org_root(&codesense_home, &Principal::from(user_id), &org).ok()?;
    let source_repos = crate::domain::load_org(org.clone(), &org_root, None)
        .org?
        .teams
        .into_iter()
        .find(|t| t.name == team)?
        .source_repos;
    Some(TeamActivation {
        codesense_home,
        user_id,
        org_slug: org,
        team,
        source_repos,
    })
}

/// Whether `(org, team)` — as an agent passed it — is already the active team,
/// so a switch to it would only re-run the activation it already had.
pub(crate) fn is_active_team(
    active: Option<&crate::mcp::state::ActiveTeam>,
    org: &str,
    team: &str,
) -> bool {
    active.is_some_and(|a| a.org_slug == org.trim() && a.team_name == team.trim())
}

/// Ask the UI to show the team the backend just switched to.
pub(crate) fn emit_active_team_switch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    switched: &ActiveTeamSwitched,
) {
    use tauri::Emitter;
    if let Err(e) = app.emit(ACTIVE_TEAM_SWITCH_EVENT, switched) {
        log::warn!("{ACTIVE_TEAM_SWITCH_EVENT} emit failed: {e}");
    }
}

// ---- describe_setup ---------------------------------------------------------

/// This machine's attribution `Doctor`: the one read behind `describe_setup` and the `setup`
/// live query.
pub(crate) fn local_doctor(
    home: &std::path::Path,
    path: Option<&std::ffi::OsStr>,
    notice: crate::attribution::install::ReplacementNotice,
) -> crate::attribution::launch::Doctor {
    crate::attribution::launch::doctor_with(home, path, notice)
}

/// [`local_doctor`] for the active local profile, off the async threads. A passive read: it
/// peeks at the replacement notice, which only `describe_setup` acknowledges.
pub(crate) async fn read_local_doctor<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<crate::attribution::launch::Doctor, String> {
    let (home, _) = crate::commands::route_to_context::resolve_ambient(app).await?;
    crate::commands::await_blocking("setup.attribution", move || {
        Ok(local_doctor(
            &home,
            std::env::var_os("PATH").as_deref(),
            crate::attribution::install::ReplacementNotice::Peek,
        ))
    })
    .await
}

/// Read setup through the active local profile and team activation lock.
pub(crate) async fn describe_setup<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<SetupState, rmcp::ErrorData> {
    use tauri::Manager;
    let watcher = app.try_state::<crate::commands::watch::WatcherState>();
    let _activation = match watcher.as_ref() {
        Some(watcher) => Some(watcher.lock_team_activation().await),
        None => None,
    };
    let (home, user_id) = crate::commands::route_to_context::resolve_ambient(app)
        .await
        .map_err(|m| rmcp::ErrorData::invalid_params(m, None))?;
    describe_setup_at(app, home, user_id, std::env::var_os("PATH")).await
}

/// The canonical async/blocking setup producer, also usable with an explicit temp
/// home and PATH in tests. The production caller holds the team activation lock.
/// Reading attribution acknowledges only a pending diagnostic replacement notice.
pub(crate) async fn describe_setup_at<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: PathBuf,
    user_id: u64,
    path: Option<std::ffi::OsString>,
) -> Result<SetupState, rmcp::ErrorData> {
    use tauri::Manager;
    let (orgs, scratch, attribution_doctor) =
        crate::commands::await_blocking("mcp.describe_setup", move || {
            let principal = Principal::from(user_id);
            let attribution_doctor = local_doctor(
                &home,
                path.as_deref(),
                crate::attribution::install::ReplacementNotice::Acknowledge,
            );
            let orgs = describe_orgs(&home, &principal)?;
            let scratch = describe_scratch(&home, &principal, &synced_org_slugs(&home, user_id)?);
            Ok((orgs, scratch, attribution_doctor))
        })
        .await
        .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
    let active = app
        .try_state::<super::state::ActiveTeamState>()
        .and_then(|state| state.get());
    Ok(SetupState {
        active_org: active.as_ref().map(|a| a.org_slug.clone()),
        active_team: active.map(|a| a.team_name),
        orgs,
        scratch,
        attribution: attribution_doctor,
    })
}

/// The setup checklist: every local org with its teams and their repos, which
/// team is active, and what the user's scratch space holds.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupState {
    pub active_org: Option<String>,
    pub active_team: Option<String>,
    pub orgs: Vec<SetupOrg>,
    pub scratch: ScratchSetup,
    /// Line-attribution capture (git-ai) on this machine.
    pub attribution: crate::attribution::launch::Doctor,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupOrg {
    pub slug: String,
    pub display_name: String,
    pub org_root: String,
    pub remote_url: Option<String>,
    /// Registry files that failed to load; when non-empty, `teams` is empty
    /// because the org could not be composed.
    pub load_errors: Vec<String>,
    pub teams: Vec<SetupTeam>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupTeam {
    pub name: String,
    pub archived: bool,
    pub parent_team: Option<String>,
    /// The team's declared starter page (`teams.yml` `landing_page:`), relative
    /// to its content folder; absent ⇒ the README/index fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starter_page: Option<String>,
    /// Whether the team's content is present locally.
    pub synced: bool,
    pub source_repos: Vec<SetupRepo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupRepo {
    /// `owner/repo`.
    pub slug: String,
    pub remote_key: String,
    pub attachment: AttachmentStatus,
    /// The bound local checkout, when one is recorded.
    pub local_path: Option<String>,
}

/// Whether a source repo is bound to a local checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AttachmentStatus {
    /// Bound, and the checkout exists.
    Attached,
    /// Bound, but the recorded checkout is gone.
    Missing,
    /// No local checkout recorded.
    Unattached,
}

/// Every local org of `principal`, each with its teams and their repos'
/// attachment status. Reads the filesystem, so async callers
/// keep it on the blocking pool.
pub(crate) fn describe_orgs(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<Vec<SetupOrg>, String> {
    let attachments_path =
        crate::context_repo::path_to_user_attachments_file(codesense_home, principal);
    // An unreadable attachments file reads as "nothing attached" rather than
    // failing the whole checklist; the repos then report `unattached`.
    let attachments = crate::context_repo::attachments_io::read_attachments(&attachments_path)
        .unwrap_or_default();
    let by_remote = crate::commands::attachments::group_attachments_by_remote(&attachments);

    let summaries = crate::commands::org::discover_local_orgs_impl(codesense_home, principal)?;
    Ok(summaries
        .into_iter()
        .map(|summary| {
            let org_root = PathBuf::from(&summary.org_root);
            let loaded = crate::domain::load_org(summary.slug.clone(), &org_root, None);
            let teams = loaded
                .org
                .map(|org| {
                    org.teams
                        .iter()
                        .map(|team| describe_team(&org_root, team, &by_remote))
                        .collect()
                })
                .unwrap_or_default();
            SetupOrg {
                slug: summary.slug,
                display_name: summary.display_name,
                org_root: summary.org_root,
                remote_url: summary.remote_url,
                load_errors: loaded
                    .file_errors
                    .iter()
                    .map(|e| format!("{}: {}", e.path.display(), e.message))
                    .collect(),
                teams,
            }
        })
        .collect())
}

fn describe_team(
    org_root: &Path,
    team: &Team,
    by_remote: &HashMap<String, Attachment>,
) -> SetupTeam {
    let content_dir = path_to_team_content_dir(org_root, &team.name);
    SetupTeam {
        name: team.name.clone(),
        archived: team.archived,
        parent_team: team.parent_team.clone(),
        starter_page: team.landing_page.clone(),
        synced: content_dir.is_dir(),
        source_repos: team
            .source_repos
            .iter()
            .map(|repo| describe_repo(repo, by_remote))
            .collect(),
    }
}

/// A repo with the local clone bound to it, if any — the attachment status
/// `describe_setup` and `list_source_repos` both report.
pub(crate) fn describe_repo(
    repo: &SourceRepoSlug,
    by_remote: &HashMap<String, Attachment>,
) -> SetupRepo {
    let remote_key = repo.remote_key();
    let bound = by_remote.get(&remote_key);
    let attachment = match bound {
        Some(a) if Path::new(&a.repo_path).is_dir() => AttachmentStatus::Attached,
        Some(_) => AttachmentStatus::Missing,
        None => AttachmentStatus::Unattached,
    };
    SetupRepo {
        slug: repo.to_string(),
        remote_key,
        attachment,
        local_path: bound.map(|a| a.repo_path.clone()),
    }
}

// ---- the scratch space ------------------------------------------------------

/// The user's scratch space as `describe_setup` reports it: what is filed for
/// each synced org, and the inert items that apply in every org.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchSetup {
    /// One entry per org synced on this Mac (the orgs a scratch item can be
    /// filed under), whether or not anything is filed there yet.
    pub orgs: Vec<ScratchOrgSetup>,
    pub all_orgs: AllOrgsCounts,
    pub repo: Option<ScratchRepoSetup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchRepoSetup {
    pub path: String,
    pub last_commit_at: Option<i64>,
    pub uncommitted: usize,
    pub error: Option<String>,
}

fn private_edits(root: &Path, prefix: &str) -> Result<usize, String> {
    let set = crate::commands::git_sync::publish_set_impl(root, None, Some(prefix))?;
    Ok(set.general.count + set.buckets.iter().map(|b| b.count).sum::<usize>())
}

/// One org's scratch space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchOrgSetup {
    pub org: String,
    /// Whether the space has been created (the first item filed creates it).
    pub exists: bool,
    pub counts: ScratchOrgCounts,
    /// Teams of this org with "Use my scratch items" switched off: nothing filed
    /// here (nor in all-orgs) applies or runs in them.
    pub off_teams: Vec<String>,
    /// The scratch settings file is damaged: EVERY team of the org reads off
    /// until it is fixed or deleted.
    pub settings_damaged: bool,
    pub uncommitted: usize,
    pub tracked: bool,
}

/// What is filed in one org's scratch space, by kind. Each applies and runs in
/// that org only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchOrgCounts {
    pub notes: usize,
    /// Files that apply over the org's teams' content (skills, docs, …),
    /// as plain files.
    pub overlay_files: usize,
    /// Private right panes (`panes/<id>.html`).
    pub panes: usize,
    pub routines: usize,
    pub agents: usize,
}

/// The all-orgs folder by inert kind. `notApplied` counts files there that
/// are not an allowlisted inert file, so no org ever reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AllOrgsCounts {
    pub skills: usize,
    pub prompts: usize,
    pub snippets: usize,
    pub templates: usize,
    pub not_applied: usize,
}

/// The orgs synced on this Mac (each has a team whose content is here): the
/// orgs a scratch item can be filed under. Sorted, no duplicates. Blocking.
pub(crate) fn synced_org_slugs(home: &Path, user_id: u64) -> Result<Vec<String>, String> {
    let orgs: std::collections::BTreeSet<String> =
        crate::commands::route_to_context::synced_contexts_in(home, user_id)?
            .into_iter()
            .map(|c| c.org_slug)
            .collect();
    Ok(orgs.into_iter().collect())
}

/// The scratch space of `principal` for every org in `synced_orgs`, plus the
/// all-orgs folder. Reads the filesystem only (never creates a space), so
/// async callers keep it on the blocking pool.
pub(crate) fn describe_scratch(
    codesense_home: &Path,
    principal: &Principal,
    synced_orgs: &[String],
) -> ScratchSetup {
    use crate::context_repo::scratch_space;
    let root = scratch_space::scratch_root(codesense_home, principal);
    let git = git2::Repository::open(&root).ok();
    let orgs: Vec<ScratchOrgSetup> = synced_orgs
        .iter()
        .map(|org| {
            let space = scratch_space::org_space(codesense_home, principal, org);
            let off = scratch_space::teams_with_scratch_off(codesense_home, principal, org);
            ScratchOrgSetup {
                org: org.clone(),
                exists: space.is_some(),
                counts: space
                    .map(|space| scratch_org_counts(org, &space))
                    .unwrap_or_default(),
                off_teams: off.teams,
                settings_damaged: off.damaged,
                uncommitted: git
                    .as_ref()
                    .and_then(|_| private_edits(&root, &format!("orgs/{org}/content")).ok())
                    .unwrap_or(0),
                tracked: scratch_space::content_baseline_exists(&root, org),
            }
        })
        .collect();
    let uncommitted = orgs.iter().map(|o| o.uncommitted).sum();
    ScratchSetup {
        repo: Principal::parse_local(principal.as_str())
            .ok()
            .map(|_| ScratchRepoSetup {
                path: root.to_string_lossy().into_owned(),
                last_commit_at: git
                    .as_ref()
                    .and_then(|r| r.head().ok()?.peel_to_commit().ok())
                    .map(|c| c.time().seconds()),
                uncommitted,
                error: if root.join(".git").exists() && git.is_none() {
                    Some("Couldn't open the private git repo".into())
                } else {
                    None
                },
            }),
        orgs,
        all_orgs: all_orgs_counts(&scratch_space::scratch_all_orgs_root(
            codesense_home,
            principal,
        )),
    }
}

fn scratch_org_counts(
    org: &str,
    space: &crate::context_repo::scratch_space::ScratchOrgSpace,
) -> ScratchOrgCounts {
    let content_dir = space.content_dir();
    let config_dir = space.config_dir();
    ScratchOrgCounts {
        notes: crate::commands::scratch::note_count(&space.notes_dir()).unwrap_or(0),
        overlay_files: crate::context_repo::scratch_space::regular_files(&content_dir).count(),
        panes: crate::panes::list_layer(
            &space.panes_dir(),
            crate::panes::PaneScope::Private,
            org,
            None,
        )
        .len(),
        routines: crate::routines::discover::space_routine_dirs(&config_dir)
            .iter()
            .filter(|(_, _, def)| def.is_ok())
            .count(),
        agents: readable_defs(crate::agents::defs::scan(&crate::agents::defs::agents_dir(
            &config_dir,
        ))),
    }
}

/// How many of a scan's definitions read cleanly (a malformed one is not
/// counted, as no list shows it).
fn readable_defs<T, E>(defs: Vec<(PathBuf, Result<T, E>)>) -> usize {
    defs.iter().filter(|(_, def)| def.is_ok()).count()
}

/// Every file in the all-orgs folder, by the inert kind it applies as — the
/// same per-file read gate every fold that consumes the folder applies.
fn all_orgs_counts(root: &Path) -> AllOrgsCounts {
    use crate::context_repo::scratch_space::{inert_kind, is_inert_file, InertKind};
    let mut counts = AllOrgsCounts::default();
    for (abs, logical) in crate::context_repo::scratch_space::regular_files(root) {
        let kind = inert_kind(&logical).filter(|_| is_inert_file(&abs, &logical));
        match kind {
            Some(InertKind::Skill) => counts.skills += 1,
            Some(InertKind::Prompt) => counts.prompts += 1,
            Some(InertKind::Snippet) => counts.snippets += 1,
            Some(InertKind::Template) => counts.templates += 1,
            None => counts.not_applied += 1,
        }
    }
    counts
}

// ---- source repos -----------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddSourceRepoParams {
    /// The context-repo org slug (`owner/repo`).
    pub org: String,
    /// The team to add the repo to.
    pub team: String,
    /// A remote to track: `https://…`, `git@…`, `host/owner/repo`, or GitHub
    /// shorthand `owner/repo`. Pass this or `path`, not both.
    #[serde(default)]
    pub url: Option<String>,
    /// An absolute path to a folder on this machine: tracked as the repo its
    /// git `origin` names (`local/<folder>` without one) and bound as that
    /// repo's local clone. Pass this or `url`, not both.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttachSourceRepoPathParams {
    pub org: String,
    pub team: String,
    /// One of the team's source repos: `owner/repo`, or `host/owner/repo`
    /// when the same `owner/repo` is on two forges.
    pub repo: String,
    /// Absolute path to a git working tree whose `origin` is `repo`.
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RemoveSourceRepoParams {
    pub org: String,
    pub team: String,
    /// One of the team's source repos: `owner/repo` or `host/owner/repo`.
    pub repo: String,
}

/// What a removal left behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovedSourceRepo {
    pub remote_key: String,
    /// The clone whose binding was forgotten; it is still on disk. `None` when
    /// no clone was bound, or another team still tracks the repo and keeps it.
    pub unbound_clone: Option<String>,
}

/// A refusal as the tool's envelope: an unknown team is `unauthorizedTeam`, an
/// I/O failure is a hard error, anything else is a `validation` the agent can
/// correct.
fn refusal<T>(error: AddRepoError) -> Reply<T> {
    match error {
        AddRepoError::UnknownTeam { .. } => Ok(ToolEnvelope::UnauthorizedTeam),
        AddRepoError::Failure { message } => Err(message),
        other => Ok(ToolEnvelope::Validation {
            message: other.to_string(),
        }),
    }
}

/// `add_source_repo`: track a URL, or track and bind a local folder.
pub(crate) async fn add_source_repo<R>(
    env: &AttachmentEnv<'_, R>,
    params: AddSourceRepoParams,
) -> Reply<TrackedSourceRepo>
where
    R: tauri::Runtime,
{
    let blank = |v: &Option<String>| v.as_deref().is_none_or(|s| s.trim().is_empty());
    let input = match (blank(&params.url), blank(&params.path)) {
        (false, true) => AddRepoInput::Url {
            url: params.url.unwrap_or_default(),
        },
        (true, false) => AddRepoInput::LocalPath {
            path: params.path.unwrap_or_default(),
        },
        _ => {
            return Ok(ToolEnvelope::Validation {
                message: "pass exactly one of url or path".to_string(),
            })
        }
    };
    match add_source_repo_to_team(env, &params.org, &params.team, input).await {
        Ok(added) => Ok(ToolEnvelope::Ok(added)),
        Err(e) => refusal(e),
    }
}

/// The team's tracked repo `repo` names, read on the blocking pool.
async fn tracked_repo(
    home: &Path,
    user_id: u64,
    org: &str,
    team: &str,
    repo: &str,
) -> Result<SourceRepoSlug, AddRepoError> {
    let (home, org, team, repo) = (
        home.to_path_buf(),
        org.trim().to_string(),
        team.trim().to_string(),
        repo.to_string(),
    );
    crate::commands::await_blocking("mcp.tracked_repo", move || {
        Ok(
            team_source_repos(&home, &Principal::from(user_id), &org, &team)
                .and_then(|tracked| resolve_tracked_repo(&tracked, &repo, &team)),
        )
    })
    .await
    .map_err(|message| AddRepoError::Failure { message })?
}

/// `attach_source_repo_path`: bind (or rebind) a local clone to a team's repo.
pub(crate) async fn attach_source_repo_path<R>(
    env: &AttachmentEnv<'_, R>,
    params: AttachSourceRepoPathParams,
) -> Reply<TrackedSourceRepo>
where
    R: tauri::Runtime,
{
    use tauri::Manager;
    let watcher = env.app.state::<crate::commands::watch::WatcherState>();
    let _activation = watcher.lock_team_activation().await;
    env.app
        .state::<crate::commands::local_profile::LocalProfileState>()
        .check_expected(Some(&env.user_id.to_string()))?;
    let slug = match tracked_repo(
        env.home,
        env.user_id,
        &params.org,
        &params.team,
        &params.repo,
    )
    .await
    {
        Ok(slug) => slug,
        Err(e) => return refusal(e),
    };
    match crate::commands::source_repo_add::attach_source_repo_clone_locked(
        env,
        &slug,
        &params.path,
    )
    .await
    {
        Ok(bound) => Ok(ToolEnvelope::Ok(bound)),
        Err(e) => refusal(e),
    }
}

/// `remove_source_repo`: stop the team tracking the repo and forget its bound
/// clone. The clone itself stays on disk.
pub(crate) async fn remove_source_repo<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    user_id: u64,
    params: RemoveSourceRepoParams,
) -> Reply<RemovedSourceRepo> {
    use tauri::Manager;
    let watcher = app.state::<crate::commands::watch::WatcherState>();
    let _activation = watcher.lock_team_activation().await;
    app.state::<crate::commands::local_profile::LocalProfileState>()
        .check_expected(Some(&user_id.to_string()))?;
    let slug = match tracked_repo(home, user_id, &params.org, &params.team, &params.repo).await {
        Ok(slug) => slug,
        Err(e) => return refusal(e),
    };
    let (app, home, org, team) = (
        app.clone(),
        home.to_path_buf(),
        params.org.trim().to_string(),
        params.team.trim().to_string(),
    );
    crate::commands::await_blocking("mcp.remove_source_repo", move || {
        let unbound = crate::commands::attachments::remove_source_repo_and_notify(
            &app, &home, user_id, &org, &team, &slug,
        )?;
        Ok(ToolEnvelope::Ok(RemovedSourceRepo {
            remote_key: slug.remote_key(),
            unbound_clone: unbound,
        }))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const ORG: &str = "acme/context";
    const UID: u64 = 7;

    /// A local org under `home`: `backend` (synced, two repos), `frontend`
    /// (listed, no local content) and `legacy` (archived).
    fn seed_org(home: &Path) -> PathBuf {
        let org_root = path_to_org_root(home, &Principal::from(UID), ORG).unwrap();
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
            "teams:\n  - name: backend\n    source_repos:\n      - owner: acme\n        repo: api\n      - owner: acme\n        repo: worker\n  - name: frontend\n    landing_page: guide/start.md\n  - name: legacy\n    archived: true\n",
        )
        .unwrap();
        std::fs::create_dir_all(path_to_team_content_dir(&org_root, "backend")).unwrap();
        // `.git` marks the directory as a cloned context repo for discovery.
        git2::Repository::init(&org_root).unwrap();
        org_root
    }

    fn params(org: &str, team: &str) -> SetActiveTeamParams {
        SetActiveTeamParams {
            org: org.into(),
            team: team.into(),
        }
    }

    /// Run `set_active_team`, recording what reached the activation path.
    async fn switch(
        home: &Path,
        p: SetActiveTeamParams,
    ) -> (ToolEnvelope<ActiveTeamSwitched>, Vec<TeamActivation>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let reply = set_active_team(home.to_path_buf(), UID, p, |activation| async move {
            sink.lock().unwrap().push(activation);
            Ok(())
        })
        .await
        .unwrap();
        let seen = seen.lock().unwrap().clone();
        (reply, seen)
    }

    #[tokio::test]
    async fn switching_runs_the_activation_path_with_the_teams_repos() {
        let home = tempfile::tempdir().unwrap();
        seed_org(home.path());

        let (reply, seen) = switch(home.path(), params(ORG, "backend")).await;

        assert_eq!(
            reply,
            ToolEnvelope::Ok(ActiveTeamSwitched {
                org_slug: ORG.into(),
                team: "backend".into(),
            })
        );
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].org_slug, ORG);
        assert_eq!(seen[0].team, "backend");
        assert_eq!(seen[0].user_id, UID);
        assert_eq!(seen[0].codesense_home, home.path());
        let repos: Vec<String> = seen[0].source_repos.iter().map(|r| r.to_string()).collect();
        assert_eq!(repos, ["acme/api", "acme/worker"]);
    }

    #[tokio::test]
    async fn a_listed_team_without_local_content_is_still_switchable() {
        let home = tempfile::tempdir().unwrap();
        seed_org(home.path());

        let (reply, seen) = switch(home.path(), params(ORG, "frontend")).await;

        assert!(matches!(reply, ToolEnvelope::Ok(_)));
        assert_eq!(seen.len(), 1);
        assert!(seen[0].source_repos.is_empty());
    }

    #[tokio::test]
    async fn an_unknown_archived_or_foreign_team_never_reaches_activation() {
        let home = tempfile::tempdir().unwrap();
        seed_org(home.path());

        for p in [
            params(ORG, "ghost"),
            params(ORG, "legacy"),
            params("other/org", "backend"),
            params("../escape", "backend"),
        ] {
            let (reply, seen) = switch(home.path(), p).await;
            assert_eq!(reply, ToolEnvelope::UnauthorizedTeam);
            assert!(seen.is_empty());
        }
    }

    #[tokio::test]
    async fn blank_args_are_a_validation_refusal() {
        let home = tempfile::tempdir().unwrap();
        let (reply, seen) = switch(home.path(), params(" ", "backend")).await;
        assert!(matches!(reply, ToolEnvelope::Validation { .. }));
        assert!(seen.is_empty());
    }

    #[tokio::test]
    async fn a_failed_activation_is_a_hard_error() {
        let home = tempfile::tempdir().unwrap();
        seed_org(home.path());
        let reply = set_active_team(
            home.path().to_path_buf(),
            UID,
            params(ORG, "backend"),
            |_| async { Err("wave engine unavailable".to_string()) },
        )
        .await;
        assert_eq!(reply, Err("wave engine unavailable".to_string()));
    }

    #[test]
    fn only_the_exact_active_team_counts_as_already_active() {
        use crate::mcp::state::ActiveTeam;
        let active = ActiveTeam {
            org_slug: ORG.into(),
            team_name: "backend".into(),
        };
        assert!(is_active_team(Some(&active), ORG, "backend"));
        assert!(is_active_team(
            Some(&active),
            &format!(" {ORG} "),
            " backend "
        ));
        assert!(!is_active_team(Some(&active), ORG, "frontend"));
        assert!(!is_active_team(Some(&active), "other/org", "backend"));
        assert!(!is_active_team(None, ORG, "backend"));
    }

    #[test]
    fn the_switch_event_carries_org_and_team() {
        use tauri::Listener;
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let sink = seen.clone();
        app.listen(ACTIVE_TEAM_SWITCH_EVENT, move |event| {
            sink.lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        emit_active_team_switch(
            app.handle(),
            &ActiveTeamSwitched {
                org_slug: ORG.into(),
                team: "backend".into(),
            },
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec![serde_json::json!({ "orgSlug": ORG, "team": "backend" })]
        );
    }

    #[test]
    fn describe_orgs_reports_teams_repos_and_attachments() {
        let home = tempfile::tempdir().unwrap();
        seed_org(home.path());
        let checkout = home.path().join("code").join("api");
        std::fs::create_dir_all(&checkout).unwrap();
        let attachments =
            crate::context_repo::path_to_user_attachments_file(home.path(), &Principal::from(UID));
        std::fs::write(
            &attachments,
            format!(
                "attachments:\n  - repo_path: {}\n    remote_url: https://github.com/acme/api.git\n  - repo_path: /nowhere/worker\n    remote_url: https://github.com/acme/worker.git\n",
                checkout.display()
            ),
        )
        .unwrap();

        let orgs = describe_orgs(home.path(), &Principal::from(UID)).unwrap();

        assert_eq!(orgs.len(), 1);
        let org = &orgs[0];
        assert_eq!(org.slug, ORG);
        assert!(org.load_errors.is_empty(), "{:?}", org.load_errors);
        let names: Vec<&str> = org.teams.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["backend", "frontend", "legacy"]);

        let backend = &org.teams[0];
        assert!(backend.synced);
        assert!(!backend.archived);
        let repos: Vec<(&str, AttachmentStatus)> = backend
            .source_repos
            .iter()
            .map(|r| (r.slug.as_str(), r.attachment))
            .collect();
        assert_eq!(
            repos,
            [
                ("acme/api", AttachmentStatus::Attached),
                ("acme/worker", AttachmentStatus::Missing),
            ]
        );
        assert_eq!(
            backend.source_repos[0].local_path.as_deref(),
            Some(checkout.to_str().unwrap())
        );
        assert!(!org.teams[1].synced);
        assert_eq!(backend.starter_page, None);
        assert_eq!(org.teams[1].starter_page.as_deref(), Some("guide/start.md"));
        assert!(org.teams[2].archived);
    }

    #[test]
    fn describe_orgs_reports_attached_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let root = crate::context_repo::context_attachments::tests::checkout(
            tmp.path(),
            "checkout",
            "acme/context",
        );
        let who = Principal::local_guest();
        crate::context_repo::context_attachments::attach(&home, &who, &root).unwrap();
        git2::Repository::open(&root)
            .unwrap()
            .remote("origin", "https://bob:ghp_x@github.com/o/r.git") // trufflehog:ignore
            .unwrap();

        let orgs = describe_orgs(&home, &who).unwrap();

        assert_eq!(orgs.len(), 1);
        assert_eq!(
            orgs[0].remote_url.as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert!(!serde_json::to_string(&orgs).unwrap().contains("ghp_x"));
    }

    fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        crate::commands::source_repo_add::tests::mock_app()
    }

    fn backend_repo(home: &Path, slug: &str) -> Option<SetupRepo> {
        describe_orgs(home, &Principal::from(UID)).unwrap()[0]
            .teams
            .iter()
            .find(|t| t.name == "backend")
            .unwrap()
            .source_repos
            .iter()
            .find(|r| r.slug == slug)
            .cloned()
    }

    #[tokio::test]
    async fn add_attach_list_remove_round_trip_keeps_the_clone() {
        let home = tempfile::tempdir().unwrap();
        seed_org(home.path());
        let app = mock_app();
        let coord = crate::content_engine::sync::WaveCoordinator::default();
        let active = crate::mcp::state::ActiveTeamState::default();
        let env = AttachmentEnv {
            app: app.handle(),
            coord: &coord,
            active_team: &active,
            home: home.path(),
            user_id: UID,
        };
        let clone = home.path().join("code").join("web");
        std::fs::create_dir_all(&clone).unwrap();
        git2::Repository::init(&clone)
            .unwrap()
            .remote("origin", "https://github.com/acme/web.git")
            .unwrap();
        let clone = clone.canonicalize().unwrap();

        let added = add_source_repo(
            &env,
            AddSourceRepoParams {
                org: ORG.into(),
                team: "backend".into(),
                url: Some("acme/web".into()),
                path: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(added, ToolEnvelope::Ok(ref r) if r.remote_key == "github.com/acme/web"));
        assert_eq!(
            backend_repo(home.path(), "acme/web").unwrap().attachment,
            AttachmentStatus::Unattached
        );

        let bound = attach_source_repo_path(
            &env,
            AttachSourceRepoPathParams {
                org: ORG.into(),
                team: "backend".into(),
                repo: "acme/web".into(),
                path: clone.to_str().unwrap().into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(bound, ToolEnvelope::Ok(_)), "{bound:?}");
        let listed = backend_repo(home.path(), "acme/web").unwrap();
        assert_eq!(listed.attachment, AttachmentStatus::Attached);
        assert_eq!(listed.local_path.as_deref(), clone.to_str());

        let removed = remove_source_repo(
            app.handle(),
            home.path(),
            UID,
            RemoveSourceRepoParams {
                org: ORG.into(),
                team: "backend".into(),
                repo: "acme/web".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            removed,
            ToolEnvelope::Ok(RemovedSourceRepo {
                remote_key: "github.com/acme/web".into(),
                unbound_clone: Some(clone.to_str().unwrap().into()),
            })
        );
        assert!(backend_repo(home.path(), "acme/web").is_none());
        assert!(clone.join(".git").is_dir(), "the clone stays on disk");
        let attachments = crate::context_repo::attachments_io::read_attachments(
            &crate::context_repo::path_to_user_attachments_file(home.path(), &Principal::from(UID)),
        )
        .unwrap();
        assert!(attachments.attachments.is_empty());
    }

    #[tokio::test]
    async fn queued_attach_revalidates_tracking_after_activation_lock() {
        use tauri::Manager;
        let home = tempfile::tempdir().unwrap();
        seed_org(home.path());
        let app = mock_app();
        let coord = crate::content_engine::sync::WaveCoordinator::default();
        let active = crate::mcp::state::ActiveTeamState::default();
        let env = AttachmentEnv {
            app: app.handle(),
            coord: &coord,
            active_team: &active,
            home: home.path(),
            user_id: UID,
        };
        let clone = home.path().join("api");
        git2::Repository::init(&clone)
            .unwrap()
            .remote("origin", "https://github.com/acme/api.git")
            .unwrap();
        let watcher = app.state::<crate::commands::watch::WatcherState>();
        let guard = watcher.lock_team_activation().await;
        let pending = attach_source_repo_path(
            &env,
            AttachSourceRepoPathParams {
                org: ORG.into(),
                team: "backend".into(),
                repo: "acme/api".into(),
                path: clone.to_str().unwrap().into(),
            },
        );
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut pending)
                .await
                .is_err()
        );
        crate::commands::attachments::remove_source_repo_and_notify(
            app.handle(),
            home.path(),
            UID,
            ORG,
            "backend",
            &SourceRepoSlug::new("acme", "api").unwrap(),
        )
        .unwrap();
        drop(guard);
        assert!(matches!(
            pending.await.unwrap(),
            ToolEnvelope::Validation { .. }
        ));
        let attachments =
            crate::context_repo::path_to_user_attachments_file(home.path(), &Principal::from(UID));
        assert!(
            crate::context_repo::attachments_io::read_attachments(&attachments)
                .unwrap()
                .attachments
                .is_empty()
        );
    }

    #[tokio::test]
    async fn source_repo_tools_refuse_bad_targets_as_envelopes() {
        let home = tempfile::tempdir().unwrap();
        seed_org(home.path());
        let app = mock_app();
        let coord = crate::content_engine::sync::WaveCoordinator::default();
        let active = crate::mcp::state::ActiveTeamState::default();
        let env = AttachmentEnv {
            app: app.handle(),
            coord: &coord,
            active_team: &active,
            home: home.path(),
            user_id: UID,
        };
        let add = |team: &str, url: Option<&str>, path: Option<&str>| AddSourceRepoParams {
            org: ORG.into(),
            team: team.into(),
            url: url.map(Into::into),
            path: path.map(Into::into),
        };

        for params in [
            add("backend", Some("acme/x"), Some("/tmp")),
            add("backend", None, None),
            add("backend", Some("acme/api"), None),
        ] {
            let reply = add_source_repo(&env, params).await.unwrap();
            assert!(
                matches!(reply, ToolEnvelope::Validation { .. }),
                "{reply:?}"
            );
        }
        for team in ["ghost", "legacy"] {
            let reply = add_source_repo(&env, add(team, Some("acme/x"), None))
                .await
                .unwrap();
            assert_eq!(reply, ToolEnvelope::UnauthorizedTeam);
        }

        let untracked = remove_source_repo(
            app.handle(),
            home.path(),
            UID,
            RemoveSourceRepoParams {
                org: ORG.into(),
                team: "backend".into(),
                repo: "acme/nope".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(untracked, ToolEnvelope::Validation { .. }));
    }

    #[test]
    fn private_summary_counts_a_rename_once_like_changes() {
        use crate::context_repo::scratch_space;
        let home = tempfile::tempdir().unwrap();
        let me = Principal::from(42u64);
        let (space, _) = scratch_space::ensure_org_space(home.path(), &me, "acme/a").unwrap();
        std::fs::write(
            space.content_dir().join("old.md"),
            "A private document that moved to another filename.\n",
        )
        .unwrap();
        scratch_space::ensure_content_baseline(home.path(), &me, "acme/a").unwrap();
        std::fs::rename(
            space.content_dir().join("old.md"),
            space.content_dir().join("new.md"),
        )
        .unwrap();
        let root = scratch_space::scratch_root(home.path(), &me);
        let repo = git2::Repository::open(&root).unwrap();
        let mut index = repo.index().unwrap();
        index
            .remove_path(Path::new("orgs/acme/a/content/old.md"))
            .unwrap();
        index
            .add_path(Path::new("orgs/acme/a/content/new.md"))
            .unwrap();
        index.write().unwrap();
        let summary = describe_scratch(home.path(), &me, &["acme/a".into()]);
        assert_eq!(summary.orgs[0].uncommitted, 1);
        assert_eq!(summary.repo.unwrap().uncommitted, 1);
    }

    /// describe_setup's scratch section counts each synced org's space by
    /// kind and the all-orgs folder by inert kind, and never creates a space.
    #[test]
    fn the_scratch_space_is_described_per_org_and_for_all_orgs() {
        use crate::context_repo::scratch_space;
        let home = tempfile::tempdir().unwrap();
        let me = Principal::from(UID);
        scratch_space::ensure_org_space(home.path(), &me, "acme/a").unwrap();
        let space = scratch_space::org_space(home.path(), &me, "acme/a").unwrap();
        let content = space.content_dir();
        std::fs::create_dir_all(content.join(".claude/skills/mine")).unwrap();
        std::fs::write(content.join(".claude/skills/mine/SKILL.md"), "mine").unwrap();
        std::fs::write(content.join("CLAUDE.md"), "more").unwrap();
        let panes = space.panes_dir();
        std::fs::create_dir_all(&panes).unwrap();
        std::fs::write(panes.join("pulls.html"), "<p>pulls</p>").unwrap();
        let notes = space.notes_dir();
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(
            notes.join("index.json"),
            r#"[{"id":"a1","title":"One","pinned":false,"lastOpenedAt":0}]"#,
        )
        .unwrap();
        std::fs::write(notes.join("a1.md"), "One").unwrap();

        let all_orgs = scratch_space::scratch_all_orgs_root(home.path(), &me);
        for (rel, body) in [
            ("skills/review/SKILL.md", "review"),
            ("prompts/standup.md", "standup"),
            ("prompts/notes.txt", "notes"),
            ("templates/pr.md", "pr"),
            ("prompts/runs.md", "!`rm -rf /`"),
            ("settings.json", "{}"),
        ] {
            let path = all_orgs.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }

        scratch_space::set_team_uses_scratch(home.path(), &me, "acme/a", "backend", false).unwrap();
        let described = describe_scratch(home.path(), &me, &["acme/a".into(), "acme/b".into()]);
        assert_eq!(
            described.orgs[0],
            ScratchOrgSetup {
                org: "acme/a".into(),
                exists: true,
                counts: ScratchOrgCounts {
                    notes: 1,
                    overlay_files: 2,
                    panes: 1,
                    routines: 0,
                    agents: 0,
                },
                off_teams: vec!["backend".into()],
                settings_damaged: false,
                uncommitted: 2,
                tracked: false,
            }
        );
        assert_eq!(
            described.orgs[1],
            ScratchOrgSetup {
                org: "acme/b".into(),
                exists: false,
                counts: ScratchOrgCounts::default(),
                off_teams: Vec::new(),
                settings_damaged: false,
                uncommitted: 0,
                tracked: false,
            }
        );
        assert!(scratch_space::org_space(home.path(), &me, "acme/b").is_none());
        assert_eq!(
            described.all_orgs,
            AllOrgsCounts {
                skills: 1,
                prompts: 2,
                snippets: 0,
                templates: 1,
                not_applied: 2,
            }
        );
        let wire = serde_json::to_value(&described).unwrap();
        assert_eq!(wire["orgs"][0]["counts"]["overlayFiles"], 2);
        assert_eq!(wire["allOrgs"]["notApplied"], 2);
    }

    #[test]
    fn a_repo_with_no_recorded_checkout_is_unattached() {
        let repo = SourceRepoSlug::parse_owner_repo("acme/api").unwrap();
        let status = describe_repo(&repo, &HashMap::new());
        assert_eq!(status.attachment, AttachmentStatus::Unattached);
        assert_eq!(status.local_path, None);
    }
    #[test]
    fn startup_capture_status_through_describe_setup_is_retained_per_home() {
        use crate::attribution::turns::tests::{replay_log, replay_repo};
        let f = crate::context_repo::caller_scope::tests::AppFixture::new();
        let home = &f.fixture.home;
        let repo = replay_repo(true);
        crate::attribution::why_line::tests::attach(home, repo.path());
        crate::attribution::prefs::update(home, &Principal::from(1), "github.com/acme/api", |p| {
            p.mode = Some(crate::attribution::prefs::AttributionMode::On);
        })
        .unwrap();
        let log = replay_log(repo.path());
        std::fs::write(log.parent().unwrap().join(".archived_at"), "1").unwrap();
        let read = || {
            serde_json::to_value(
                tauri::async_runtime::block_on(describe_setup(f.app.handle())).unwrap(),
            )
            .unwrap()
        };
        assert!(read()["attribution"]["capture"]["lastSweepAt"].is_null());
        crate::attribution::catch_up::run_at(home, &home.join("usage.sqlite"));
        let snapshot = read()["attribution"]["capture"].clone();
        assert!(snapshot["lastSweepAt"].is_u64());
        assert_eq!(snapshot["trigger"], "startup");
        assert_eq!(snapshot["clonesSwept"], 1);
        assert_eq!(snapshot["logsSeen"], 1);
        assert_eq!(snapshot["logsNew"], 1);
        assert_eq!(snapshot["traceRowsStored"], 2);
        assert_eq!(snapshot["sessionMappingRowsStored"], 2);
        assert!(snapshot["lastError"].is_null());
        assert_eq!(
            read()["attribution"]["capture"],
            snapshot,
            "a setup read never sweeps"
        );
        let other = tempfile::tempdir().unwrap();
        assert!(
            serde_json::to_value(crate::attribution::launch::doctor(other.path(), None)).unwrap()
                ["capture"]["lastSweepAt"]
                .is_null()
        );
        crate::attribution::catch_up::run_at(home, &home.join("usage.sqlite"));
        let unchanged = read()["attribution"]["capture"].clone();
        assert_eq!(unchanged["logsSeen"], 1);
        assert_eq!(unchanged["logsNew"], 0);
        assert_eq!(unchanged["traceRowsStored"], 0);
        assert_eq!(unchanged["sessionMappingRowsStored"], 0);
        let blocked = home.join("blocked");
        std::fs::write(&blocked, "file").unwrap();
        crate::attribution::catch_up::run_at(home, &blocked.join("usage.sqlite"));
        let error = read()["attribution"]["capture"]["lastError"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(error, "checkpoint store unavailable");
        assert!(!error.contains(&home.to_string_lossy().to_string()));
    }
    #[test]
    fn review_partial_committed_capture_survives_later_failure_through_describe_setup() {
        use crate::attribution::turns::tests::{replay_log, replay_repo};
        let f = crate::context_repo::caller_scope::tests::AppFixture::new();
        let home = &f.fixture.home;
        let repo = replay_repo(true);
        crate::attribution::why_line::tests::attach(home, repo.path());
        crate::attribution::prefs::set_for_checkout(
            home,
            &Principal::from(1),
            repo.path().to_str().unwrap(),
            Some(crate::attribution::prefs::AttributionChoice::On),
            None,
        )
        .unwrap();
        let first = replay_log(repo.path());
        std::fs::write(first.parent().unwrap().join(".archived_at"), "1").unwrap();
        let second = first
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("old-{}", "a".repeat(40)));
        std::fs::create_dir_all(&second).unwrap();
        std::fs::copy(&first, second.join("checkpoints.jsonl")).unwrap();
        std::fs::write(second.join(".archived_at"), "1").unwrap();
        let db = home.join("usage.sqlite");
        let _store = crate::session_usage::UsageStore::open(&db).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        // Independent of filesystem enumeration: permit one file transaction, abort the next.
        conn.execute_batch("CREATE TRIGGER fail_later_file BEFORE INSERT ON checkpoint_log_cursor WHEN (SELECT COUNT(*) FROM checkpoint_log_cursor) >= 1 BEGIN SELECT RAISE(ABORT, 'later checkpoint file failed'); END;").unwrap();
        let read = || {
            serde_json::to_value(
                tauri::async_runtime::block_on(describe_setup(f.app.handle())).unwrap(),
            )
            .unwrap()["attribution"]["capture"]
                .clone()
        };
        crate::attribution::catch_up::run_at(home, &db);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM checkpoint_log_cursor", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM checkpoint_trace", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let partial = read();
        assert_eq!(partial["logsSeen"], 2);
        assert_eq!(
            partial["logsNew"], 1,
            "describe_setup must retain the first committed file's counts after a later file fails"
        );
        assert_eq!(partial["traceRowsStored"], 2);
        assert_eq!(partial["sessionMappingRowsStored"], 2);
        assert_eq!(partial["lastError"], "checkpoint import failed");
        let error = partial["lastError"].as_str().unwrap();
        assert!(!error.contains(home.to_str().unwrap()));
        assert!(!error.contains(repo.path().to_str().unwrap()));
        crate::attribution::catch_up::run_at(home, &db);
        let retry = read();
        assert_eq!(retry["logsSeen"], 2);
        assert_eq!(retry["logsNew"], 0);
        assert_eq!(retry["traceRowsStored"], 0);
        assert_eq!(retry["sessionMappingRowsStored"], 0);
        assert_eq!(retry["lastError"], "checkpoint import failed");
        conn.execute_batch("DROP TRIGGER fail_later_file").unwrap();
        crate::attribution::catch_up::run_at(home, &db);
        let recovered = read();
        assert_eq!(recovered["logsNew"], 1);
        assert_eq!(recovered["traceRowsStored"], 2);
        assert_eq!(recovered["sessionMappingRowsStored"], 2);
        assert!(recovered["lastError"].is_null());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM checkpoint_log_cursor", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        crate::attribution::catch_up::run_at(home, &db);
        assert_eq!(read()["logsNew"], 0);
        assert_eq!(read()["traceRowsStored"], 0);
    }
}
