// SPDX-License-Identifier: MIT
//! Read authority comes from the caller, independently of the selected target.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::Manager;

use super::inheritance::{self, Admit, TeamView};
use crate::commands::dashboard_launch::LaunchTeam;
use crate::domain::Principal;

pub(crate) const MISSING: &str = "File not found";

/// Architecture: execute existing mutation policy, then expose only admitted readbacks.
pub(crate) fn readback<T>(readable: bool, result: Result<T, String>) -> Result<T, String> {
    if readable {
        result
    } else {
        Err(MISSING.into())
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) enum CallerScope {
    #[default]
    Human,
    Team(Arc<TeamView>),
    Unknown,
}

impl CallerScope {
    pub(crate) fn team(home: &Path, principal: &Principal, team: &LaunchTeam) -> Self {
        let Ok(root) = super::path_to_org_root(home, principal, &team.org) else {
            return Self::Unknown;
        };
        Self::Team(inheritance::team_view(&root, &team.org, &team.team))
    }

    /// Architecture: dynamic DOM has its viewer's authority, independent of source sharing.
    pub(crate) fn admits_viewer(&self, viewer: &CallerScope) -> bool {
        match (self, viewer) {
            (Self::Human, _) => true,
            (Self::Team(_), Self::Team(view)) => view
                .own_content()
                .own_path()
                .and_then(Path::parent)
                .is_some_and(|space| self.allows(&space.join("config"))),
            _ => false,
        }
    }

    pub(crate) fn cache_key(&self) -> serde_json::Value {
        match self {
            Self::Human => serde_json::json!(["human"]),
            Self::Unknown => serde_json::json!(["unknown"]),
            Self::Team(view) => serde_json::json!([
                "team",
                view.org(),
                view.reader_team(),
                view.own_content().own_path().map(Path::to_path_buf)
            ]),
        }
    }

    pub(crate) fn allows(&self, path: &Path) -> bool {
        match self {
            Self::Human => true,
            Self::Team(view) => view.admits_path(path) != Admit::Denied,
            Self::Unknown => inheritance::unknown_caller_admits(path),
        }
    }

    /// [`Self::allows`] for the real path of an index record, through a per-query memo.
    pub(crate) fn allows_real(&self, memo: &mut inheritance::ScopeMemo, real: PathBuf) -> bool {
        match self {
            Self::Human => true,
            Self::Team(view) => view.admits_real(memo, real) != Admit::Denied,
            Self::Unknown => memo.unknown_caller_admits_real(&real),
        }
    }

    pub(crate) fn visible(&self, path: &Path) -> bool {
        self.allows(path) || matches!(self, Self::Team(view) if view.visible_directory(path))
    }

    pub(crate) fn allows_team_content(
        &self,
        home: &Path,
        principal: &Principal,
        org: &str,
        team: &str,
        logical: &str,
    ) -> bool {
        if matches!(self, Self::Human) {
            return true;
        }
        super::path_to_org_root(home, principal, org)
            .ok()
            .is_some_and(|root| {
                inheritance::team_view(&root, org, team)
                    .own_content()
                    .own_path()
                    .is_some_and(|content| self.allows(&content.join(logical)))
            })
    }

    pub(crate) fn target_team(
        &self,
        home: &Path,
        principal: &Principal,
        org: &str,
        team: &str,
    ) -> bool {
        if matches!(self, Self::Human) {
            return true;
        }
        match crate::agents::roots::team_agents_dir(home, principal, org, team) {
            Ok(Some(path)) => self.allows(&path),
            // Architecture: a principal's private namespace exists before its first save.
            Ok(None) => super::scratch_space::is_scratch_team(team),
            Err(_) => false,
        }
    }

    /// Architecture: preparation must not mint an ancestor's session authority.
    /// The existing launch spine owns its target team; refuse before preparation
    /// when it cannot carry the child's authority through that spine.
    pub(crate) fn require_launch_team(
        &self,
        home: &Path,
        principal: &Principal,
        org: &str,
        team: &str,
    ) -> Result<(), String> {
        self.target_team(home, principal, org, team)
            .then_some(())
            .ok_or_else(|| MISSING.into())
    }

    pub(crate) fn require_launch_key(
        &self,
        home: &Path,
        principal: &Principal,
        team_key: &str,
    ) -> Result<(), String> {
        if matches!(self, Self::Human) {
            return Ok(());
        }
        let (org, team) = crate::commands::pty_layout::parse_team_key(team_key);
        self.require_launch_team(home, principal, org, team.ok_or(MISSING)?)
    }

    pub(crate) fn require_agent(
        &self,
        home: &Path,
        principal: &Principal,
        key: &crate::agents::AgentKey,
    ) -> Result<(), String> {
        self.definition(home, principal, key)
            .then_some(())
            .ok_or_else(|| MISSING.into())
    }

    pub(crate) async fn context<R: tauri::Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
    ) -> Result<crate::commands::route_to_context::ActiveContext, String> {
        use crate::commands::route_to_context::{self, ActiveContext};
        match self {
            Self::Human => route_to_context::derive_active_context(app).await,
            Self::Unknown => Err(MISSING.into()),
            Self::Team(view) => {
                let (home, user) = route_to_context::resolve_ambient(app).await?;
                Ok(ActiveContext::for_team(
                    home,
                    user,
                    view.org().into(),
                    view.reader_team().into(),
                ))
            }
        }
    }

    /// Architecture: bind a page-created agent before its MCP bridge is exposed.
    pub(crate) fn record_page_launch<R: tauri::Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        home: &Path,
        principal: &Principal,
        session: &str,
        cwd: &Path,
        target_team: Option<&str>,
        launcher: Option<&str>,
    ) -> Result<(), String> {
        if matches!(self, Self::Human) {
            return Ok(());
        }
        let Self::Team(view) = self else {
            return Err(MISSING.into());
        };
        if let Some(team) = target_team {
            self.require_launch_key(home, principal, team)?;
        }
        if let Some(owner) = workspace_team(home, principal, cwd) {
            self.require_launch_team(home, principal, &owner.org, &owner.team)?;
        } else if let Some(Self::Team(owner)) = for_content(cwd) {
            self.require_launch_team(home, principal, owner.org(), owner.reader_team())?;
        }
        let registry = app
            .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
            .ok_or(MISSING)?;
        if !registry
            .register_owned(session, session, launcher, None)
            .map_err(|_| MISSING.to_string())?
        {
            return Err(MISSING.into());
        }
        let team = LaunchTeam {
            org: view.org().into(),
            team: view.reader_team().into(),
        };
        registry.set_launch_team_if_absent(session, team.clone());
        if registry.launch_team(session).as_ref() != Some(&team) {
            return Err(MISSING.into());
        }
        registry.set_worktree_dir_if_absent(session, cwd.to_path_buf());
        Ok(())
    }

    /// Gate all definition reads used for a dispatch, before admission or warnings.
    pub(crate) fn require_routine(
        &self,
        home: &Path,
        principal: &Principal,
        key: &crate::routines::RoutineKey,
    ) -> Result<(), String> {
        if matches!(self, Self::Human) {
            return Ok(());
        }
        if !self.routine_definition(home, principal, key) {
            return Err(MISSING.into());
        }
        let def = crate::routines::service::readable_def_for(home, principal, key, self)
            .map_err(|_| MISSING.to_string())?;
        if !def.is_command() {
            if let Ok(agent) = def.agent.key_for(key) {
                self.require_agent(home, principal, &agent)?;
            }
        }
        Ok(())
    }

    pub(crate) fn routine_definition(
        &self,
        home: &Path,
        principal: &Principal,
        key: &crate::routines::RoutineKey,
    ) -> bool {
        if matches!(self, Self::Human) {
            return true;
        }
        crate::routines::service::key_config_dir(home, principal, key)
            .is_ok_and(|path| self.allows(&path))
    }

    pub(crate) fn definition(
        &self,
        home: &Path,
        principal: &Principal,
        key: &crate::agents::AgentKey,
    ) -> bool {
        if matches!(self, Self::Human) || key.is_builtin() {
            return true;
        }
        crate::agents::roots::team_agents_dir(home, principal, &key.org, &key.team)
            .ok()
            .flatten()
            .is_some_and(|dir| {
                self.allows(&dir.join(&key.id).join(crate::agents::defs::AGENT_FILE))
            })
    }

    pub(crate) fn require(&self, path: &Path) -> Result<(), String> {
        if self.allows(path) {
            Ok(())
        } else {
            Err(MISSING.into())
        }
    }

    /// Architecture: a persistent source consumer must retain caller authority or refuse before preparation.
    pub(crate) fn require_preparation_path(&self, path: &Path) -> Result<(), String> {
        self.require(path)?;
        if matches!(self, Self::Team(view) if matches!(view.admits_path(path), Admit::Inherited { .. }))
        {
            Err(MISSING.into())
        } else {
            Ok(())
        }
    }

    /// Architecture: admission happens before any bytes or filesystem diagnostics.
    pub(crate) fn read_admitted(&self, path: &Path) -> Result<String, String> {
        codesense_fs_core::FsService::read(self, path).map_err(|error| match self {
            Self::Human => error,
            _ => MISSING.into(),
        })
    }

    pub(crate) fn existing(&self, path: &Path) -> Result<(), String> {
        self.require(path)?;
        path.exists().then_some(()).ok_or_else(|| MISSING.into())
    }

    pub(crate) fn selected_view(
        &self,
        selected: Arc<TeamView>,
        selected_team: &str,
    ) -> Result<Arc<TeamView>, String> {
        match self {
            Self::Team(view)
                if view.org() == selected.org()
                    && view.inherited.iter().any(|l| l.team() == selected_team) =>
            {
                Ok(view.clone())
            }
            Self::Unknown => Err(MISSING.into()),
            _ => Ok(selected),
        }
    }
}

impl codesense_fs_core::FsGate for CallerScope {
    fn admit(&self, path: &Path) -> Result<(), String> {
        self.require(path)
    }
}

/// A recorded session never borrows the team's currently selected UI state.
pub(crate) async fn for_session<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session: Option<&str>,
    remote: Option<LaunchTeam>,
) -> CallerScope {
    let home = crate::commands::paths::codesense_home_dir();
    let profile = crate::commands::local_profile::peek_active(app);
    let (known, launch, workspace) = session
        .and_then(|session| {
            let state =
                app.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()?;
            let handle = state
                .handle_for_session(session)
                .or_else(|| state.remote_handle_for_session(session))
                .unwrap_or_else(|| session.into());
            let known = state.has_handle(&handle);
            let launch = state.launch_team(&handle);
            let workspace = if known && launch.is_none() {
                state
                    .snapshot()
                    .into_iter()
                    .find(|row| row.handle == handle)
                    .and_then(|row| row.worktree_dir)
            } else {
                None
            };
            Some((known, launch, workspace))
        })
        .unwrap_or((false, None, None));
    let worktree_team = if let (Some(cwd), Some(home), Some(profile)) =
        (workspace, home.clone(), profile.as_ref())
    {
        let principal = profile.principal.clone();
        crate::commands::await_blocking("caller_scope.worktree", move || {
            Ok::<_, String>(workspace_team(&home, &principal, &cwd))
        })
        .await
        .ok()
        .flatten()
    } else {
        None
    };
    let Some(team) = launch.or(worktree_team).or(remote) else {
        return if known {
            CallerScope::Unknown
        } else {
            CallerScope::Human
        };
    };
    let Some(home) = home else {
        return CallerScope::Unknown;
    };
    let Some(profile) = profile else {
        return CallerScope::Unknown;
    };
    CallerScope::team(&home, &profile.principal, &team)
}

pub(crate) fn workspace_team(home: &Path, principal: &Principal, cwd: &Path) -> Option<LaunchTeam> {
    use crate::install::worktree_index;
    if let Ok(Some(record)) = worktree_index::read_record(home, principal, cwd) {
        return Some(LaunchTeam {
            org: record.org_slug.clone(),
            team: worktree_index::resolve_team(&record)?,
        });
    }
    let canonical = |path: &Path| {
        path.canonicalize()
            .unwrap_or_else(|_| inheritance::normalize(path))
    };
    let cwd = canonical(cwd);
    worktree_index::context_records(home, principal)
        .ok()?
        .into_iter()
        .find(|r| canonical(&r.launch_cwd) == cwd || canonical(&r.root) == cwd)
        .map(|r| LaunchTeam {
            org: r.org,
            team: r.team,
        })
}

pub(crate) fn for_content(path: &Path) -> Option<CallerScope> {
    inheritance::view_for_content(path)
        .filter(|view| !super::scratch_space::is_scratch_team(view.reader_team()))
        .map(CallerScope::Team)
}

/// A frame's host team takes precedence over the owning team of an inherited pane.
pub(crate) fn for_page<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    page: Option<&str>,
    frame_id: Option<&str>,
) -> Result<CallerScope, String> {
    if let Some(id) = frame_id {
        let frame = app
            .try_state::<Arc<crate::dashboard_stream::frame::FrameStore>>()
            .and_then(|s| s.by_id(id))
            .ok_or(MISSING)?;
        if let Some(parent) = frame.parent_frame.as_deref() {
            return for_page(app, None, Some(parent));
        }
        if !matches!(frame.grant.slot.as_str(), "dock" | "file-nav") {
            if let Some(scope) = for_content(Path::new(&frame.content_root)) {
                return Ok(scope);
            }
        }
        if let Some(team) = frame.grant.team {
            return Ok(from_app_team(
                app,
                &LaunchTeam {
                    org: team.org,
                    team: team.team,
                },
            ));
        }
        if let Some(scope) = for_content(Path::new(&frame.content_root)) {
            return Ok(scope);
        }
    }
    if let Some(scope) = page.and_then(|p| for_content(Path::new(p))) {
        return Ok(scope);
    }
    Ok(app
        .try_state::<crate::mcp::state::ActiveTeamState>()
        .and_then(|s| s.get())
        .map(|t| {
            from_app_team(
                app,
                &LaunchTeam {
                    org: t.org_slug,
                    team: t.team_name,
                },
            )
        })
        .unwrap_or(CallerScope::Human))
}

/// Architecture: native host identity is independent of the selected content checkout.
pub(crate) fn for_host<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    page: Option<&str>,
    frame: Option<&str>,
    team_key: Option<&str>,
) -> Result<CallerScope, String> {
    if frame.is_some() || team_key.is_none() {
        return for_page(app, page, frame);
    }
    let Some((org, team)) = team_key.and_then(|key| key.rsplit_once(':')) else {
        return Ok(CallerScope::Unknown);
    };
    Ok(from_app_team(
        app,
        &LaunchTeam {
            org: org.into(),
            team: team.into(),
        },
    ))
}

/// Native management stays cross-team; page readers retain their host scope.
pub(crate) fn for_management_view<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    page: Option<&str>,
    frame_id: Option<&str>,
) -> Result<CallerScope, String> {
    if page.is_some() || frame_id.is_some() {
        for_page(app, page, frame_id)
    } else {
        Ok(CallerScope::Human)
    }
}

fn from_app_team<R: tauri::Runtime>(app: &tauri::AppHandle<R>, team: &LaunchTeam) -> CallerScope {
    match (
        crate::commands::paths::codesense_home_dir(),
        crate::commands::local_profile::peek_active(app),
    ) {
        (Some(home), Some(profile)) => CallerScope::team(&home, &profile.principal, team),
        _ => CallerScope::Unknown,
    }
}

#[cfg(test)]
pub(crate) mod tests;

impl crate::agents::content_reader::ContentReader for CallerScope {
    fn node(&self, path: &Path) -> std::io::Result<crate::agents::content_reader::Node> {
        if !self.visible(path) {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        crate::agents::content_reader::FsContentReader.node(path)
    }
    fn list(&self, path: &Path) -> std::io::Result<Vec<std::ffi::OsString>> {
        if !self.visible(path) {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        Ok(crate::agents::content_reader::FsContentReader
            .list(path)?
            .into_iter()
            .filter(|name| self.visible(&path.join(name)))
            .collect())
    }
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn std::io::Read>> {
        self.require(path)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        crate::agents::content_reader::FsContentReader.open(path)
    }
    fn file_len(&self, path: &Path) -> std::io::Result<u64> {
        self.require(path)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        crate::agents::content_reader::FsContentReader.file_len(path)
    }
}
