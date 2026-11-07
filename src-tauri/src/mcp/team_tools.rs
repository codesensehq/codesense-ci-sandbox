// SPDX-License-Identifier: MIT
//! The team setup MCP tools: create, rename, archive, delete and re-parent a
//! team, set its starter page, and rename the workspace (the org's display name).
//!
//! Each tool is one [`RegistryEdit`] committed through the same write path the
//! UI uses ([`commit_registry_edit`]), so the registry rules (team-name shape,
//! duplicates, reserved names, unknown teams) are the ones `context_repo::mutate`
//! enforces, and every view hears the `org-registry-changed` event that path
//! emits. A refused edit writes nothing and comes back as a `validation`
//! envelope carrying the registry's own message. There are no ownership checks:
//! any teammate may edit a team.

use std::future::Future;
use std::path::Path;

use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::registry_local::{commit_registry_edit_for_profile, OrgRegistryChanged};
use crate::context_repo::RegistryEdit;
use crate::domain::Principal;

use super::setup_tools::{ActiveTeamSwitched, Reply, SetActiveTeamParams};
use super::tool_envelope::ToolEnvelope;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateTeamParams {
    /// The context-repo org slug (`owner/repo`) to create the team in.
    pub org: String,
    /// The new team's name (lowercase slug).
    pub name: String,
    /// An existing team to nest the new team under.
    #[serde(default)]
    pub parent: Option<String>,
    /// Also switch the app to the new team, as `set_active_team` does.
    #[serde(default)]
    pub activate: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenameTeamParams {
    /// The context-repo org slug (`owner/repo`).
    pub org: String,
    /// The team's current name.
    pub team: String,
    /// The team's new name (lowercase slug).
    pub new_name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveTeamParams {
    /// The context-repo org slug (`owner/repo`).
    pub org: String,
    /// The team to archive or unarchive.
    pub team: String,
    /// `true` archives the team, `false` unarchives it.
    pub archived: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteTeamParams {
    /// The context-repo org slug (`owner/repo`).
    pub org: String,
    /// The team to remove from the org's registry.
    pub team: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetTeamParentParams {
    /// The context-repo org slug (`owner/repo`).
    pub org: String,
    /// The team to re-parent.
    pub team: String,
    /// The new parent team; omit (or null) to make the team a root.
    #[serde(default)]
    pub parent: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetTeamStarterPageParams {
    /// The context-repo org slug (`owner/repo`). Omit with `team` to use the
    /// active team.
    #[serde(default)]
    pub org: Option<String>,
    /// The team whose starter page to set. Omit with `org` to use the active team.
    #[serde(default)]
    pub team: Option<String>,
    /// Required. The file to open whenever the team has nothing open: a path
    /// relative to the team's content folder (`guide/start.md`), or an absolute
    /// path inside it. Pass `null` to clear it, so the team falls back to
    /// README.md, README.html, then index.html.
    #[serde(default, deserialize_with = "explicit_value")]
    #[schemars(with = "Option<String>")]
    pub path: Option<Option<String>>,
}

/// `Some(value)` whenever the key is present (`Some(None)` for an explicit
/// `null`); a missing key stays `None` (via `default`), so leaving `path` out
/// never clears the page by accident.
fn explicit_value<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// What `set_team_starter_page` did: the registry change and the page now set
/// (relative to the team's content folder; `null` once cleared).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StarterPageSet {
    #[serde(flatten)]
    pub change: OrgRegistryChanged,
    pub starter_page: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenameWorkspaceParams {
    /// The context-repo org slug (`owner/repo`).
    pub org: String,
    /// The workspace's new display name.
    pub name: String,
}

/// What `create_team` did: the registry change, and whether the app switched to
/// the new team (with why not, when an asked-for switch failed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamCreated {
    #[serde(flatten)]
    pub change: OrgRegistryChanged,
    pub activated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_error: Option<String>,
    /// Said when the new parent shares nothing with its child teams.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// What `set_team_parent` did: the registry change, plus a warning when the new
/// parent has no `config/sharing.yml` and so shares nothing with the team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamParentSet {
    #[serde(flatten)]
    pub change: OrgRegistryChanged,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// The profile the registry records for whoever creates a team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeamCreator {
    pub id: u64,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

/// The team creator for `user_id` — the ambient user every registry write
/// resolves to. The one place a creator profile is derived; the auth status
/// only supplies its display fields.
pub(crate) async fn current_team_creator(
    app: &tauri::AppHandle,
    user_id: u64,
) -> Result<TeamCreator, String> {
    let profile = crate::commands::local_profile::active(app)?;
    if profile.principal.numeric_id()? != user_id {
        return Err("Local profile changed".into());
    }
    Ok(TeamCreator {
        id: user_id,
        username: profile.display_name.clone(),
        display_name: Some(profile.display_name),
        avatar_url: None,
    })
}

/// Commit one registry edit to `org`: `Ok(Err(message))` when the registry
/// refused it (nothing written), `Err` only when the app state is missing.
async fn commit_edit<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    principal: &Principal,
    org: &str,
    edit: RegistryEdit,
) -> Result<Result<OrgRegistryChanged, String>, String> {
    let change = OrgRegistryChanged::new(principal, org, &edit);
    Ok(
        commit_registry_edit_for_profile(app, home, principal, org, edit)
            .await
            .map(|_| change),
    )
}

/// Commit one registry edit to `org`. A refused edit is a `validation` envelope
/// with the registry's message; nothing was written.
pub(crate) async fn commit_team_edit<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    principal: &Principal,
    org: &str,
    edit: RegistryEdit,
) -> Reply<OrgRegistryChanged> {
    Ok(match commit_edit(app, home, principal, org, edit).await? {
        Ok(change) => ToolEnvelope::Ok(change),
        Err(message) => ToolEnvelope::Validation { message },
    })
}

/// Set or clear a team's parent. Nesting under a parent with no
/// `config/sharing.yml` succeeds and warns that it shares nothing; the file is
/// the parent's to write, so it is never created here.
pub(crate) async fn set_team_parent<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    principal: &Principal,
    params: SetTeamParentParams,
) -> Reply<TeamParentSet> {
    let parent = params.parent.clone();
    let (org, edit) = params.into_edit();
    Ok(match commit_edit(app, home, principal, &org, edit).await? {
        Ok(change) => ToolEnvelope::Ok(TeamParentSet {
            change,
            warnings: nesting_warnings(home, principal, &org, parent).await,
        }),
        Err(message) => ToolEnvelope::Validation { message },
    })
}

/// The warning for nesting under `parent`, read off the async executor: the
/// parent shares nothing when it has no `config/sharing.yml`. Best-effort: an
/// org root that will not resolve warns nothing.
async fn nesting_warnings(
    home: &Path,
    principal: &Principal,
    org: &str,
    parent: Option<String>,
) -> Vec<String> {
    let Some(parent) = parent else {
        return Vec::new();
    };
    let Ok(root) = crate::context_repo::path_to_org_root(home, principal, org) else {
        return Vec::new();
    };
    crate::commands::await_blocking("mcp.team_parent.sharing", move || {
        Ok(crate::context_repo::inheritance::nesting_warning(
            &root, &parent,
        ))
    })
    .await
    .ok()
    .flatten()
    .into_iter()
    .collect()
}

/// Create a team as `user_id`. A `parent` is checked against the registry
/// first, so a bad parent writes nothing; the team is then created and nested.
/// With `activate`, `switch` (the app's team switch) runs last; if it fails the
/// team stays created and the reply says why it is not active.
pub(crate) async fn create_team<R, F, Fut>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    creator: TeamCreator,
    params: CreateTeamParams,
    switch: F,
) -> Reply<TeamCreated>
where
    R: tauri::Runtime,
    F: FnOnce(SetActiveTeamParams) -> Fut,
    Fut: Future<Output = Reply<ActiveTeamSwitched>>,
{
    let principal = Principal::from(creator.id);
    let name = params.name.trim().to_string();
    let refused = |message| Ok(ToolEnvelope::Validation { message });

    if let Some(parent) = &params.parent {
        if let Err(message) = check_parent(home, &principal, &params.org, &name, parent).await? {
            return refused(message);
        }
    }
    let create = RegistryEdit::CreateTeam {
        team_name: name.clone(),
        creator_id: creator.id,
        creator_username: creator.username,
        creator_display_name: creator.display_name,
        creator_avatar_url: creator.avatar_url,
    };
    let change = match commit_edit(app, home, &principal, &params.org, create).await? {
        Ok(change) => change,
        Err(message) => return refused(message),
    };
    if let Some(parent) = params.parent.clone() {
        let nest = RegistryEdit::SetTeamParent {
            team_name: name.clone(),
            parent_team: Some(parent),
        };
        // Only a concurrent edit to the parent can refuse this after the check.
        if let Err(message) = commit_edit(app, home, &principal, &params.org, nest).await? {
            return refused(format!(
                "team '{name}' was created, but not nested: {message}"
            ));
        }
    }
    let warnings = nesting_warnings(home, &principal, &params.org, params.parent).await;
    let activation_error = if params.activate {
        let switched = switch(SetActiveTeamParams {
            org: params.org,
            team: name,
        })
        .await;
        switch_failure(switched)
    } else {
        None
    };
    Ok(ToolEnvelope::Ok(TeamCreated {
        change,
        activated: params.activate && activation_error.is_none(),
        activation_error,
        warnings,
    }))
}

/// The registry's verdict on `parent` for the team `name`, read off the async
/// executor. A malformed `org` is left to the create edit to refuse.
async fn check_parent(
    home: &Path,
    principal: &Principal,
    org: &str,
    name: &str,
    parent: &str,
) -> Result<Result<(), String>, String> {
    if crate::context_repo::org_slug::require_valid_org_slug(org).is_err() {
        return Ok(Ok(()));
    }
    let org_root = crate::context_repo::path_to_org_root(home, principal, org)?;
    let (name, parent) = (name.to_string(), parent.to_string());
    crate::commands::await_blocking("mcp.create_team.check_parent", move || {
        Ok(
            crate::context_repo::check_team_parent(&org_root, &name, &parent)
                .map_err(|e| e.to_string()),
        )
    })
    .await
}

pub(crate) async fn set_team_starter_page_for<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    principal: &Principal,
    active: Option<(String, String)>,
    params: SetTeamStarterPageParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Reply<StarterPageSet> {
    let selected = match (&params.org, &params.team) {
        (Some(org), Some(team)) => Some((org.as_str(), team.as_str())),
        (None, None) => active
            .as_ref()
            .map(|(org, team)| (org.as_str(), team.as_str())),
        _ => None,
    };
    let readable = matches!(scope, crate::context_repo::caller_scope::CallerScope::Human)
        || match params.path.as_ref().and_then(Option::as_deref) {
            None => true,
            Some(path) => selected.is_some_and(|(org, team)| {
                scope.allows_team_content(home, principal, org, team, path.trim())
            }),
        };
    let result = set_team_starter_page(app, home, principal, active, params).await;
    crate::context_repo::caller_scope::readback(readable, result)
}

/// Set or clear a team's starter page (`teams.yml` `landing_page:`) through the
/// shared registry write path: a working-tree edit, never committed or pushed.
/// `active` is the app's active team, used when neither `org` nor `team` is given.
pub(crate) async fn set_team_starter_page<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    principal: &Principal,
    active: Option<(String, String)>,
    params: SetTeamStarterPageParams,
) -> Reply<StarterPageSet> {
    let refused = |message: String| Ok(ToolEnvelope::Validation { message });
    let (org, team) = match (params.org, params.team, active) {
        (Some(org), Some(team), _) => (org, team),
        (None, None, Some(active)) => active,
        (None, None, None) => return refused("no active team: pass `org` and `team`".into()),
        _ => return refused("pass both `org` and `team`, or neither for the active team".into()),
    };
    let Some(path) = params.path else {
        return refused(
            "pass `path`: a file in the team's content folder, or null to clear".into(),
        );
    };
    let page = match path.map(|p| p.trim().to_string()) {
        None => None,
        Some(p) if Path::new(&p).is_absolute() => {
            // Canonicalizing touches the disk: off the async executor.
            let (home, principal, org, team) = (
                home.to_path_buf(),
                principal.clone(),
                org.clone(),
                team.clone(),
            );
            let rel = crate::commands::await_blocking("mcp.set_team_starter_page", move || {
                Ok(content_relative(&home, &principal, &org, &team, &p))
            })
            .await?;
            match rel {
                Ok(rel) => Some(rel),
                Err(message) => return refused(message),
            }
        }
        Some(p) => Some(p),
    };
    let edit = RegistryEdit::SetTeamLandingPage {
        team_name: team,
        landing_page: page.clone(),
    };
    Ok(match commit_edit(app, home, principal, &org, edit).await? {
        Ok(change) => ToolEnvelope::Ok(StarterPageSet {
            change,
            starter_page: page,
        }),
        Err(message) => ToolEnvelope::Validation { message },
    })
}

/// `abs` relative to `team`'s content folder, or why it is not inside it.
fn content_relative(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
    abs: &str,
) -> Result<String, String> {
    crate::context_repo::org_slug::require_valid_org_slug(org)?;
    let org_root = crate::context_repo::path_to_org_root(home, principal, org)?;
    let content = crate::context_repo::path_to_team_content_dir(&org_root, team);
    // Lexically first, then through symlinks (`/var` ⇄ `/private/var`, a
    // symlinked home): the same file spelled either way is inside.
    let lexical = Path::new(abs)
        .strip_prefix(&content)
        .ok()
        .map(Path::to_path_buf);
    let resolved = || {
        let real_content = content.canonicalize().ok()?;
        let real = Path::new(abs).canonicalize().ok()?;
        real.strip_prefix(&real_content).ok().map(Path::to_path_buf)
    };
    lexical
        .or_else(resolved)
        .and_then(|rel| {
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            (!parts.is_empty()).then(|| parts.join("/"))
        })
        .ok_or_else(|| {
            format!(
                "{abs} is not inside team '{team}''s content folder ({})",
                content.display()
            )
        })
}

/// Why a team switch did not land, or `None` when it did.
fn switch_failure(switched: Reply<ActiveTeamSwitched>) -> Option<String> {
    match switched {
        Ok(ToolEnvelope::Ok(_)) => None,
        Ok(ToolEnvelope::Validation { message }) => Some(message),
        Ok(other) => Some(
            serde_json::to_value(&other)
                .ok()
                .and_then(|v| v["status"].as_str().map(str::to_string))
                .unwrap_or_else(|| "switch refused".to_string()),
        ),
        Err(message) => Some(message),
    }
}

impl RenameTeamParams {
    pub(crate) fn into_edit(self) -> (String, RegistryEdit) {
        (
            self.org,
            RegistryEdit::RenameTeam {
                old_name: self.team,
                new_name: self.new_name.trim().to_string(),
            },
        )
    }
}

impl ArchiveTeamParams {
    pub(crate) fn into_edit(self) -> (String, RegistryEdit) {
        (
            self.org,
            RegistryEdit::ArchiveTeam {
                team_name: self.team,
                archived: self.archived,
            },
        )
    }
}

impl DeleteTeamParams {
    pub(crate) fn into_edit(self) -> (String, RegistryEdit) {
        (
            self.org,
            RegistryEdit::DeleteTeam {
                team_name: self.team,
            },
        )
    }
}

impl SetTeamParentParams {
    pub(crate) fn into_edit(self) -> (String, RegistryEdit) {
        (
            self.org,
            RegistryEdit::SetTeamParent {
                team_name: self.team,
                parent_team: self.parent,
            },
        )
    }
}

impl RenameWorkspaceParams {
    pub(crate) fn into_edit(self) -> (String, RegistryEdit) {
        (
            self.org,
            RegistryEdit::RenameWorkspace {
                new_display_name: self.name,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use crate::commands::registry_local::ORG_REGISTRY_CHANGED_EVENT;
    use crate::context_repo::path_to_org_root;

    const ORG: &str = "acme/context";
    const UID: u64 = 42;

    type Events = Arc<Mutex<Vec<serde_json::Value>>>;

    /// A committed org under a temp codesense home with teams `platform` and
    /// `web`, and a mock app recording every `org-registry-changed` payload.
    fn fixture() -> (
        tauri::App<tauri::test::MockRuntime>,
        tempfile::TempDir,
        PathBuf,
        Events,
    ) {
        use tauri::Listener;
        let home = tempfile::tempdir().unwrap();
        let org_root = path_to_org_root(home.path(), &Principal::from(UID), ORG).unwrap();
        std::fs::create_dir_all(&org_root).unwrap();
        // The fixture commits to `refs/heads/main`, so HEAD must name it whatever the
        // global `init.defaultBranch` is (unset, or another test's temp HOME, means master).
        let repo = git2::Repository::init_opts(
            &org_root,
            git2::RepositoryInitOptions::new().initial_head("main"),
        )
        .unwrap();
        let files = [
            (
                "org.yml",
                "slug: acme\ndisplay_name: Acme\ncreated_at: 2026-01-01T00:00:00Z\nconfig_flags: {}\n",
            ),
            (
                "people.yml",
                "people:\n  - github_user_id: 42\n    github_username: creator42\n",
            ),
            (
                "teams.yml",
                "teams:\n  - name: platform\n    source_repos: []\n  - name: web\n    source_repos: []\n",
            ),
        ];
        let mut index = repo.index().unwrap();
        for (name, body) in files {
            std::fs::write(org_root.join(name), body).unwrap();
            index.add_path(Path::new(name)).unwrap();
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let app = tauri::test::mock_builder()
            .manage(crate::commands::local_profile::LocalProfileState::for_test(
                home.path(),
                UID,
                crate::commands::local_profile::MachineOwner::Local {
                    id: uuid::Uuid::new_v4().to_string(),
                },
            ))
            .manage(crate::commands::watch::WatcherState::default())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let seen: Events = Arc::default();
        let sink = seen.clone();
        app.listen(ORG_REGISTRY_CHANGED_EVENT, move |event| {
            sink.lock()
                .unwrap()
                .push(serde_json::from_str(event.payload()).unwrap());
        });
        (app, home, org_root, seen)
    }

    fn creator() -> TeamCreator {
        TeamCreator {
            id: UID,
            username: "creator42".into(),
            display_name: Some("Creator".into()),
            avatar_url: None,
        }
    }

    fn teams_yml(org_root: &Path) -> String {
        std::fs::read_to_string(org_root.join("teams.yml")).unwrap()
    }

    fn team<'a>(org: &'a crate::domain::Org, name: &str) -> Option<&'a crate::domain::Team> {
        org.teams.iter().find(|t| t.name == name)
    }

    fn load(org_root: &Path) -> crate::domain::Org {
        crate::domain::load_org(ORG.to_string(), org_root, None)
            .org
            .expect("org loads")
    }

    async fn commit(
        app: &tauri::App<tauri::test::MockRuntime>,
        home: &Path,
        (org, edit): (String, RegistryEdit),
    ) -> ToolEnvelope<OrgRegistryChanged> {
        commit_team_edit(app.handle(), home, &Principal::from(UID), &org, edit)
            .await
            .unwrap()
    }

    /// Run `create_team`, recording any switch it asks for.
    async fn create(
        app: &tauri::App<tauri::test::MockRuntime>,
        home: &Path,
        params: CreateTeamParams,
        switch_reply: ToolEnvelope<ActiveTeamSwitched>,
    ) -> (ToolEnvelope<TeamCreated>, Vec<(String, String)>) {
        let switches = Arc::new(Mutex::new(Vec::new()));
        let sink = switches.clone();
        let reply = create_team(app.handle(), home, creator(), params, |p| async move {
            sink.lock().unwrap().push((p.org, p.team));
            Ok(switch_reply)
        })
        .await
        .unwrap();
        let switches = switches.lock().unwrap().clone();
        (reply, switches)
    }

    fn create_params(name: &str, parent: Option<&str>, activate: bool) -> CreateTeamParams {
        CreateTeamParams {
            org: ORG.into(),
            name: name.into(),
            parent: parent.map(str::to_string),
            activate,
        }
    }

    fn switched(team: &str) -> ToolEnvelope<ActiveTeamSwitched> {
        ToolEnvelope::Ok(ActiveTeamSwitched {
            org_slug: ORG.into(),
            team: team.into(),
        })
    }

    #[tokio::test]
    async fn create_team_writes_the_team_and_records_the_creator() {
        let (app, home, org_root, seen) = fixture();
        let (reply, switches) = create(
            &app,
            home.path(),
            create_params("payments", None, false),
            switched("payments"),
        )
        .await;

        assert_eq!(
            serde_json::to_value(&reply).unwrap(),
            serde_json::json!({
                "status": "ok",
                "data": { "principal": UID.to_string(), "orgSlug": ORG, "edit": "CreateTeam", "team": "payments", "activated": false },
            })
        );
        assert!(switches.is_empty(), "no switch without activate");
        let org = load(&org_root);
        assert!(team(&org, "payments").is_some());
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                serde_json::json!({ "principal": UID.to_string(), "orgSlug": ORG, "edit": "CreateTeam", "team": "payments" })
            ]
        );
    }

    #[tokio::test]
    async fn create_team_nests_under_a_parent_and_activates() {
        let (app, home, org_root, seen) = fixture();
        let (reply, switches) = create(
            &app,
            home.path(),
            create_params("payments", Some("platform"), true),
            switched("payments"),
        )
        .await;

        assert!(matches!(
            reply,
            ToolEnvelope::Ok(TeamCreated {
                activated: true,
                ..
            })
        ));
        assert_eq!(switches, vec![(ORG.to_string(), "payments".to_string())]);
        let org = load(&org_root);
        assert_eq!(
            team(&org, "payments").unwrap().parent_team.as_deref(),
            Some("platform")
        );
        let edits: Vec<_> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|e| e["edit"].clone())
            .collect();
        assert_eq!(edits, ["CreateTeam", "SetTeamParent"]);
    }

    #[tokio::test]
    async fn a_failed_switch_keeps_the_team_and_says_why_it_is_not_active() {
        let (app, home, org_root, _seen) = fixture();
        let (reply, _) = create(
            &app,
            home.path(),
            create_params("payments", None, true),
            ToolEnvelope::UnauthorizedTeam,
        )
        .await;
        assert_eq!(
            serde_json::to_value(&reply).unwrap()["data"],
            serde_json::json!({
                "principal": UID.to_string(), "orgSlug": ORG, "edit": "CreateTeam", "team": "payments",
                "activated": false, "activationError": "unauthorizedTeam",
            })
        );
        assert!(team(&load(&org_root), "payments").is_some());
    }

    #[tokio::test]
    async fn a_bad_parent_refuses_the_create_and_writes_nothing() {
        let (app, home, org_root, seen) = fixture();
        let before = teams_yml(&org_root);
        let (reply, switches) = create(
            &app,
            home.path(),
            create_params("payments", Some("ghost"), true),
            switched("payments"),
        )
        .await;
        match reply {
            ToolEnvelope::Validation { message } => {
                assert!(message.contains("parent team 'ghost'"), "{message}")
            }
            other => panic!("expected validation, got {other:?}"),
        }
        assert!(switches.is_empty());
        assert_eq!(teams_yml(&org_root), before);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn set_team_parent_refuses_a_cycle_and_writes_nothing() {
        let (app, home, org_root, seen) = fixture();
        let parent = |team: &str, parent: &str| SetTeamParentParams {
            org: ORG.into(),
            team: team.into(),
            parent: Some(parent.into()),
        };
        assert!(matches!(
            commit(&app, home.path(), parent("web", "platform").into_edit()).await,
            ToolEnvelope::Ok(_)
        ));
        let before = teams_yml(&org_root);
        for (team, to) in [("platform", "web"), ("web", "web"), ("web", "ghost")] {
            assert!(
                matches!(
                    commit(&app, home.path(), parent(team, to).into_edit()).await,
                    ToolEnvelope::Validation { .. }
                ),
                "{team} -> {to}"
            );
        }
        assert_eq!(teams_yml(&org_root), before);
        assert_eq!(seen.lock().unwrap().len(), 1);
        load(&org_root);
    }

    async fn reparent(
        app: &tauri::App<tauri::test::MockRuntime>,
        home: &Path,
        parent: Option<&str>,
    ) -> serde_json::Value {
        let params = SetTeamParentParams {
            org: ORG.into(),
            team: "web".into(),
            parent: parent.map(str::to_string),
        };
        let reply = set_team_parent(app.handle(), home, &Principal::from(UID), params)
            .await
            .unwrap();
        serde_json::to_value(&reply).unwrap()
    }

    fn shares_nothing(team: &str) -> serde_json::Value {
        serde_json::json!([crate::context_repo::inheritance::shares_nothing_warning(
            team
        )])
    }

    #[tokio::test]
    async fn set_team_parent_warns_when_parent_shares_nothing() {
        let (app, home, org_root, _seen) = fixture();
        let reply = reparent(&app, home.path(), Some("platform")).await;
        assert_eq!(reply["status"], "ok", "{reply}");
        assert_eq!(reply["data"]["warnings"], shares_nothing("platform"));
        assert_eq!(
            team(&load(&org_root), "web")
                .unwrap()
                .parent_team
                .as_deref(),
            Some("platform")
        );
        assert!(!org_root.join("platform/config/sharing.yml").exists());
        assert!(!org_root.join("platform/config").exists());
        // Clearing the parent warns nothing.
        let reply = reparent(&app, home.path(), None).await;
        assert_eq!(reply["status"], "ok", "{reply}");
        assert!(reply["data"].get("warnings").is_none(), "{reply}");
    }

    #[tokio::test]
    async fn set_team_parent_onto_sharing_parent_has_no_warnings() {
        let (app, home, org_root, _seen) = fixture();
        std::fs::create_dir_all(org_root.join("platform/config")).unwrap();
        // A real share, and the explicit share-nothing, are both silent.
        for declaration in ["share: ['content/**']\n", "share: []\n"] {
            std::fs::write(org_root.join("platform/config/sharing.yml"), declaration).unwrap();
            let reply = reparent(&app, home.path(), Some("platform")).await;
            assert_eq!(reply["status"], "ok", "{reply}");
            assert_eq!(
                reply["data"],
                serde_json::json!({
                    "principal": UID.to_string(), "orgSlug": ORG, "edit": "SetTeamParent", "team": "web",
                }),
                "{declaration}"
            );
            reparent(&app, home.path(), None).await;
        }
    }

    #[tokio::test]
    async fn create_team_with_non_sharing_parent_warns() {
        let (app, home, org_root, _seen) = fixture();
        let (reply, _) = create(
            &app,
            home.path(),
            create_params("payments", Some("platform"), false),
            switched("payments"),
        )
        .await;
        let reply = serde_json::to_value(&reply).unwrap();
        assert_eq!(reply["data"]["warnings"], shares_nothing("platform"));
        assert_eq!(
            team(&load(&org_root), "payments")
                .unwrap()
                .parent_team
                .as_deref(),
            Some("platform")
        );
        assert!(!org_root.join("platform/config/sharing.yml").exists());
    }

    #[tokio::test]
    async fn create_team_onto_sharing_parent_has_no_warnings() {
        let (app, home, org_root, _seen) = fixture();
        std::fs::create_dir_all(org_root.join("platform/config")).unwrap();
        std::fs::write(
            org_root.join("platform/config/sharing.yml"),
            "share: ['content/**']\n",
        )
        .unwrap();
        let (reply, _) = create(
            &app,
            home.path(),
            create_params("payments", Some("platform"), false),
            switched("payments"),
        )
        .await;
        let reply = serde_json::to_value(&reply).unwrap();
        assert_eq!(reply["status"], "ok", "{reply}");
        assert!(reply["data"].get("warnings").is_none(), "{reply}");
        assert_eq!(
            team(&load(&org_root), "payments")
                .unwrap()
                .parent_team
                .as_deref(),
            Some("platform")
        );
    }

    /// Move the fixture org to a user checkout under `dir` and attach it, so
    /// the org resolves there and no longer under the managed path.
    async fn attach_fixture(home: &Path, org_root: &Path, dir: &Path) -> PathBuf {
        // An attached checkout's identity is its org.yml slug.
        std::fs::write(
            org_root.join("org.yml"),
            format!("slug: {ORG}\ndisplay_name: Acme\ncreated_at: 2026-01-01T00:00:00Z\nconfig_flags: {{}}\n"),
        )
        .unwrap();
        {
            let repo = git2::Repository::open(org_root).unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("org.yml")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            let sig = git2::Signature::now("test", "test@test.com").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "slug", &tree, &[&head])
                .unwrap();
        }
        let checkout = dir.join("checkout");
        std::fs::rename(org_root, &checkout).unwrap();
        // `attach` takes a blocking lock, which a runtime thread may not.
        let (owned_home, selected) = (home.to_path_buf(), checkout.clone());
        tokio::task::spawn_blocking(move || {
            crate::context_repo::context_attachments::attach(
                &owned_home,
                &Principal::from(UID),
                &selected,
            )
        })
        .await
        .unwrap()
        .unwrap();
        let root = path_to_org_root(home, &Principal::from(UID), ORG).unwrap();
        assert_eq!(root, checkout.canonicalize().unwrap());
        root
    }

    #[tokio::test]
    async fn set_team_parent_warns_on_an_attached_org() {
        let (app, home, org_root, _seen) = fixture();
        let elsewhere = tempfile::tempdir().unwrap();
        let checkout = attach_fixture(home.path(), &org_root, elsewhere.path()).await;
        let reply = reparent(&app, home.path(), Some("platform")).await;
        assert_eq!(reply["status"], "ok", "{reply}");
        assert_eq!(reply["data"]["warnings"], shares_nothing("platform"));
        assert_eq!(
            team(&load(&checkout), "web")
                .unwrap()
                .parent_team
                .as_deref(),
            Some("platform")
        );
        assert!(!checkout.join("platform/config/sharing.yml").exists());
        // The declaration is read from the checkout: sharing there silences it.
        reparent(&app, home.path(), None).await;
        std::fs::create_dir_all(checkout.join("platform/config")).unwrap();
        std::fs::write(
            checkout.join("platform/config/sharing.yml"),
            "share: ['content/**']\n",
        )
        .unwrap();
        let reply = reparent(&app, home.path(), Some("platform")).await;
        assert_eq!(reply["status"], "ok", "{reply}");
        assert!(reply["data"].get("warnings").is_none(), "{reply}");
    }

    #[tokio::test]
    async fn an_invalid_or_duplicate_create_returns_the_registry_error_and_writes_nothing() {
        let (app, home, org_root, seen) = fixture();
        let before = teams_yml(&org_root);
        for (name, expected) in [
            ("Not A Slug", "invalid team name"),
            ("web", "duplicate: team 'web' already exists"),
            ("ops-meta", "reserved `-meta` suffix"),
        ] {
            let (reply, switches) = create(
                &app,
                home.path(),
                create_params(name, None, true),
                switched(name),
            )
            .await;
            match reply {
                ToolEnvelope::Validation { message } => {
                    assert!(message.contains(expected), "{name}: {message}")
                }
                other => panic!("{name}: expected validation, got {other:?}"),
            }
            assert!(
                switches.is_empty(),
                "{name}: a refused create never switches"
            );
        }
        assert_eq!(teams_yml(&org_root), before);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rename_team_moves_the_team_and_names_both_sides() {
        let (app, home, org_root, seen) = fixture();
        let reply = commit(
            &app,
            home.path(),
            RenameTeamParams {
                org: ORG.into(),
                team: "web".into(),
                new_name: "frontend".into(),
            }
            .into_edit(),
        )
        .await;
        assert!(matches!(reply, ToolEnvelope::Ok(_)));
        let org = load(&org_root);
        assert!(team(&org, "frontend").is_some() && team(&org, "web").is_none());
        assert_eq!(
            *seen.lock().unwrap(),
            vec![serde_json::json!({
                "principal": UID.to_string(), "orgSlug": ORG, "edit": "RenameTeam", "team": "frontend", "previousTeam": "web",
            })]
        );
    }

    #[tokio::test]
    async fn archive_set_parent_rename_workspace_and_delete_round_trip() {
        let (app, home, org_root, seen) = fixture();
        let h = home.path();
        let archive = |archived| ArchiveTeamParams {
            org: ORG.into(),
            team: "web".into(),
            archived,
        };
        assert!(matches!(
            commit(&app, h, archive(true).into_edit()).await,
            ToolEnvelope::Ok(_)
        ));
        assert!(team(&load(&org_root), "web").unwrap().archived);
        assert!(matches!(
            commit(&app, h, archive(false).into_edit()).await,
            ToolEnvelope::Ok(_)
        ));
        assert!(!team(&load(&org_root), "web").unwrap().archived);

        let parent = |parent: Option<&str>| SetTeamParentParams {
            org: ORG.into(),
            team: "web".into(),
            parent: parent.map(str::to_string),
        };
        commit(&app, h, parent(Some("platform")).into_edit()).await;
        assert_eq!(
            team(&load(&org_root), "web")
                .unwrap()
                .parent_team
                .as_deref(),
            Some("platform")
        );
        commit(&app, h, parent(None).into_edit()).await;
        assert_eq!(team(&load(&org_root), "web").unwrap().parent_team, None);

        let renamed = commit(
            &app,
            h,
            RenameWorkspaceParams {
                org: ORG.into(),
                name: "Acme Corp".into(),
            }
            .into_edit(),
        )
        .await;
        assert!(matches!(renamed, ToolEnvelope::Ok(_)));
        assert_eq!(load(&org_root).display_name, "Acme Corp");

        let deleted = commit(
            &app,
            h,
            DeleteTeamParams {
                org: ORG.into(),
                team: "web".into(),
            }
            .into_edit(),
        )
        .await;
        assert!(matches!(deleted, ToolEnvelope::Ok(_)));
        assert!(team(&load(&org_root), "web").is_none());

        let edits: Vec<_> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|e| e["edit"].clone())
            .collect();
        assert_eq!(
            edits,
            [
                "ArchiveTeam",
                "ArchiveTeam",
                "SetTeamParent",
                "SetTeamParent",
                "RenameWorkspace",
                "DeleteTeam"
            ]
        );
    }

    #[tokio::test]
    async fn edits_on_an_unknown_team_or_bad_name_return_the_registry_error_and_write_nothing() {
        let (app, home, org_root, seen) = fixture();
        let before = teams_yml(&org_root);
        let ghost = || "ghost".to_string();
        let refused = [
            DeleteTeamParams {
                org: ORG.into(),
                team: ghost(),
            }
            .into_edit(),
            ArchiveTeamParams {
                org: ORG.into(),
                team: ghost(),
                archived: true,
            }
            .into_edit(),
            SetTeamParentParams {
                org: ORG.into(),
                team: ghost(),
                parent: None,
            }
            .into_edit(),
            RenameTeamParams {
                org: ORG.into(),
                team: "web".into(),
                new_name: "platform".into(),
            }
            .into_edit(),
            RenameTeamParams {
                org: ORG.into(),
                team: "web".into(),
                new_name: "Bad Name".into(),
            }
            .into_edit(),
            RenameWorkspaceParams {
                org: ORG.into(),
                name: "  ".into(),
            }
            .into_edit(),
        ];
        for edit in refused {
            let label = edit.1.kind();
            match commit(&app, home.path(), edit).await {
                ToolEnvelope::Validation { message } => assert!(!message.is_empty(), "{label}"),
                other => panic!("{label}: expected validation, got {other:?}"),
            }
        }
        assert_eq!(teams_yml(&org_root), before);
        assert!(seen.lock().unwrap().is_empty());
    }

    async fn starter(
        app: &tauri::App<tauri::test::MockRuntime>,
        home: &Path,
        active: Option<(&str, &str)>,
        org: Option<&str>,
        team: Option<&str>,
        path: Option<&str>,
    ) -> ToolEnvelope<StarterPageSet> {
        set_team_starter_page(
            app.handle(),
            home,
            &Principal::from(UID),
            active.map(|(o, t)| (o.to_string(), t.to_string())),
            SetTeamStarterPageParams {
                org: org.map(str::to_string),
                team: team.map(str::to_string),
                // `None` here is an explicit `null` (clear); omission is tested apart.
                path: Some(path.map(str::to_string)),
            },
        )
        .await
        .unwrap()
    }

    fn write_content(org_root: &Path, team: &str, rel: &str) -> PathBuf {
        let path = crate::context_repo::path_to_team_content_dir(org_root, team).join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();
        path
    }

    #[tokio::test]
    async fn set_team_starter_page_sets_and_clears_the_page_without_committing() {
        let (app, home, org_root, seen) = fixture();
        write_content(&org_root, "web", "guide/start.md");
        let head = || {
            git2::Repository::open(&org_root)
                .unwrap()
                .head()
                .unwrap()
                .target()
        };
        let before_head = head();
        let reply = starter(
            &app,
            home.path(),
            None,
            Some(ORG),
            Some("web"),
            Some("guide/start.md"),
        )
        .await;
        assert_eq!(
            serde_json::to_value(&reply).unwrap(),
            serde_json::json!({
                "status": "ok",
                "data": { "principal": UID.to_string(), "orgSlug": ORG, "edit": "SetTeamLandingPage", "team": "web", "starterPage": "guide/start.md" },
            })
        );
        assert_eq!(
            team(&load(&org_root), "web")
                .unwrap()
                .landing_page
                .as_deref(),
            Some("guide/start.md")
        );
        assert_eq!(head(), before_head, "a local working-tree edit only");

        // The active team is the default target; null clears.
        let cleared = starter(&app, home.path(), Some((ORG, "web")), None, None, None).await;
        assert!(matches!(
            cleared,
            ToolEnvelope::Ok(StarterPageSet {
                starter_page: None,
                ..
            })
        ));
        assert_eq!(team(&load(&org_root), "web").unwrap().landing_page, None);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_absolute_path_inside_the_team_content_is_made_relative() {
        let (app, home, org_root, _seen) = fixture();
        let abs = write_content(&org_root, "platform", "README.html");
        let reply = starter(
            &app,
            home.path(),
            Some((ORG, "platform")),
            None,
            None,
            Some(abs.to_str().unwrap()),
        )
        .await;
        assert!(
            matches!(&reply, ToolEnvelope::Ok(set) if set.starter_page.as_deref() == Some("README.html")),
            "{reply:?}"
        );
        let elsewhere = write_content(&org_root, "web", "README.md");
        let refused = starter(
            &app,
            home.path(),
            Some((ORG, "platform")),
            None,
            None,
            Some(elsewhere.to_str().unwrap()),
        )
        .await;
        assert!(
            matches!(refused, ToolEnvelope::Validation { .. }),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn a_bad_starter_page_or_target_writes_nothing() {
        let (app, home, org_root, seen) = fixture();
        write_content(&org_root, "web", ".env");
        let before = teams_yml(&org_root);
        for (active, org, team_name, path) in [
            (None, Some(ORG), Some("web"), Some("../../teams.yml")),
            (None, Some(ORG), Some("web"), Some(".env")),
            (None, Some(ORG), Some("web"), Some("missing.md")),
            (None, Some(ORG), Some("ghost"), None),
            (None, None, None, Some("README.md")),
            (Some((ORG, "web")), Some(ORG), None, Some("README.md")),
        ] {
            let reply = starter(&app, home.path(), active, org, team_name, path).await;
            match reply {
                ToolEnvelope::Validation { message } => assert!(!message.is_empty()),
                other => panic!("{team_name:?} {path:?}: expected validation, got {other:?}"),
            }
        }
        assert_eq!(teams_yml(&org_root), before);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn leaving_out_path_is_refused_and_never_clears() {
        let (app, home, org_root, seen) = fixture();
        let params: SetTeamStarterPageParams =
            serde_json::from_value(serde_json::json!({ "org": ORG, "team": "web" })).unwrap();
        assert_eq!(params.path, None);
        let cleared: SetTeamStarterPageParams =
            serde_json::from_value(serde_json::json!({ "path": null })).unwrap();
        assert_eq!(cleared.path, Some(None));
        let before = teams_yml(&org_root);
        let reply = set_team_starter_page(
            app.handle(),
            home.path(),
            &Principal::from(UID),
            None,
            params,
        )
        .await
        .unwrap();
        assert!(
            matches!(reply, ToolEnvelope::Validation { .. }),
            "{reply:?}"
        );
        assert_eq!(teams_yml(&org_root), before);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_codesense_docs_starter_page_is_read_only() {
        let (app, home, _org_root, seen) = fixture();
        let docs = crate::context_repo::codesense_docs::bundle_info();
        let reply = starter(
            &app,
            home.path(),
            None,
            Some(&docs.slug),
            Some(&docs.team),
            Some("tour.html"),
        )
        .await;
        match reply {
            ToolEnvelope::Validation { message } => assert_eq!(
                message,
                crate::context_repo::codesense_docs::READ_ONLY_MESSAGE
            ),
            other => panic!("expected validation, got {other:?}"),
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_switch_error_is_reported_as_the_activation_error() {
        assert_eq!(switch_failure(Err("boom".into())), Some("boom".into()));
        assert_eq!(switch_failure(Ok(switched("x"))), None);
    }
}
