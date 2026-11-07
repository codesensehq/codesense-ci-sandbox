// SPDX-License-Identifier: MIT
use super::*;

pub(crate) struct Fixture {
    pub temp: tempfile::TempDir,
    pub home: std::path::PathBuf,
    pub root: std::path::PathBuf,
    pub scope: CallerScope,
}
impl Fixture {
    pub fn new() -> Self {
        let (temp, _) = inheritance::test_fixture();
        let home = temp.path().canonicalize().unwrap().join(".codesense");
        std::fs::create_dir(&home).unwrap();
        std::fs::rename(temp.path().join("users"), home.join("users")).unwrap();
        let root = home.join("users/1/orgs/acme/ctx");
        std::fs::create_dir(root.join(".git")).unwrap();
        let scope = CallerScope::Team(inheritance::team_view(&root, "acme/ctx", "child"));
        Self {
            temp,
            home,
            root,
            scope,
        }
    }
    pub fn path(&self, team: &str, logical: &str) -> std::path::PathBuf {
        self.root.join(format!("{team}/{logical}"))
    }
    pub fn write(&self, team: &str, logical: &str, content: &str) -> std::path::PathBuf {
        let path = self.path(team, logical);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }
    pub fn roots(&self) -> Vec<crate::context_query::index::IndexRoot> {
        ["child", "parent", "grand", "sibling"]
            .into_iter()
            .map(|team| crate::context_query::index::IndexRoot {
                dir: self.path(team, "content"),
                org: Some("acme/ctx".into()),
                team: Some(team.into()),
                layer: crate::context_query::model::Layer::Team,
            })
            .collect()
    }
    pub fn agent(&self, team: &str, id: &str) -> crate::agents::AgentKey {
        crate::agents::service::save(
            &self.home,
            &Principal::from(1u64),
            crate::agents::SaveTarget::Create(crate::agents::Placement::Team {
                org: "acme/ctx".into(),
                team: team.into(),
            }),
            crate::routines::test_support::agent(
                id,
                codesense_pty_core::agent_cli::AgentCli::Claude,
            ),
            &crate::routines::Author::user("1"),
        )
        .unwrap()
        .key
    }
    pub fn routine(&self, team: &str, id: &str) -> String {
        self.write(team, &format!("config/agents/{}/routines/{id}/routine.yml", crate::routines::layout::COMMAND_DIR),
            &format!("id: {id}\nname: {team} routine\nowner: '1'\naction: command\npolicy: {{max_runtime: 5m, overlap: queue}}\ncommand: [echo, hello]\nwhere: {{kind: team_workspace}}\nrevision: 1\n"));
        format!("acme/ctx/{team}/{id}")
    }
    pub fn service(&self) -> crate::context_query::ContextIndexService {
        self.service_with(self.roots())
    }
    pub fn service_with(
        &self,
        roots: Vec<crate::context_query::index::IndexRoot>,
    ) -> crate::context_query::ContextIndexService {
        use std::sync::Arc;
        let service = crate::context_query::ContextIndexService::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        service.start(
            Arc::new(move || {
                Some(crate::context_query::service::Footprint {
                    owner: 1,
                    roots: roots.clone(),
                    summary_headings: vec![],
                })
            }),
            Arc::new(move |_| {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        service
    }
}

pub(crate) struct AppFixture {
    pub fixture: Fixture,
    pub app: tauri::App<tauri::test::MockRuntime>,
    previous: Option<std::ffi::OsString>,
    _env: std::sync::MutexGuard<'static, ()>,
}
impl AppFixture {
    pub fn new() -> Self {
        use tauri::Manager;
        let guard = crate::content_engine::test_support::env_guard();
        let fixture = Fixture::new();
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", fixture.home.parent().unwrap());
        let app = crate::routines::test_support::launch_app(&fixture.home);
        app.manage(Arc::new(
            crate::dashboard_stream::frame::FrameStore::default(),
        ));
        app.state::<crate::mcp::state::ActiveTeamState>()
            .update(crate::mcp::state::ActiveTeam {
                org_slug: "acme/ctx".into(),
                team_name: "child".into(),
            });
        Self {
            fixture,
            app,
            previous,
            _env: guard,
        }
    }
    pub fn session(&self, id: &str, team: Option<&str>) {
        use crate::commands::dashboard_launch::{DashboardLaunchState, LaunchTeam};
        if self.app.try_state::<DashboardLaunchState>().is_none() {
            self.app.manage(DashboardLaunchState::new(16));
        }
        let state = self.app.state::<DashboardLaunchState>();
        state.register(id, id, Some("codesense"));
        if let Some(team) = team {
            state.set_launch_team_if_absent(
                id,
                LaunchTeam {
                    org: "acme/ctx".into(),
                    team: team.into(),
                },
            );
        }
    }

    pub fn frame(&self, page: &Path, scope: &str) -> crate::dashboard_stream::frame::FrameRecord {
        self.frame_for_team(page, scope, "child")
    }
    pub fn frame_for_team(
        &self,
        page: &Path,
        scope: &str,
        team: &str,
    ) -> crate::dashboard_stream::frame::FrameRecord {
        use crate::dashboard_stream::frame::*;
        use tauri::Manager;
        self.app.state::<Arc<FrameStore>>().mint(
            9321,
            FrameGrant {
                frame_id: String::new(),
                principal: "1".into(),
                dashboard: page.to_string_lossy().into(),
                slot: "right-pane".into(),
                team: Some(FrameTeam {
                    org: "acme/ctx".into(),
                    team: team.into(),
                }),
                scope: scope.into(),
            },
            self.fixture.path(team, "content").to_string_lossy().into(),
            page.to_string_lossy().into(),
            page.parent().unwrap().to_string_lossy().into(),
            FramePolicy::default(),
            None,
        )
    }
}
impl Drop for AppFixture {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn recorded_worktree_fills_missing_launch_team_before_remote_or_layout() {
    use crate::install::worktree_index::{self, ContextWorktreeRecord};
    let f = Fixture::new();
    let root = f.temp.path().join("retained");
    let cwd = root.join("child/content");
    std::fs::create_dir_all(&cwd).unwrap();
    let principal = Principal::from(1u64);
    worktree_index::write_context_record(
        &f.home,
        &principal,
        &ContextWorktreeRecord {
            owner: None,
            extras: Default::default(),
            schema_version: worktree_index::SCHEMA_VERSION,
            kind: "context".into(),
            id: "retained-child".into(),
            org: "acme/ctx".into(),
            team: "child".into(),
            name: "Retained".into(),
            branch: "work/child".into(),
            base_sha: "0".repeat(40),
            root,
            launch_cwd: cwd.clone(),
            created_at: 1,
            last_used_at: 1,
        },
    )
    .unwrap();
    let recorded = workspace_team(&f.home, &principal, &cwd).unwrap();
    assert_eq!(recorded.team, "child");
    let scope = CallerScope::team(&f.home, &principal, &recorded);
    assert_eq!(
        scope.existing(&f.path("parent", "content/secret.md")),
        scope.existing(&f.path("parent", "content/missing.md"))
    );
    assert!(scope
        .existing(&f.path("parent", "content/shared.md"))
        .is_ok());
}
#[test]
fn caller_admission_hides_ancestors_but_preserves_other_teams() {
    let (temp, root) = inheritance::test_fixture();
    let scope = CallerScope::Team(inheritance::team_view(&root, "acme/ctx", "child"));
    assert_eq!(
        scope.existing(&root.join("parent/content/secret.md")),
        scope.existing(&root.join("parent/content/missing.md"))
    );
    assert!(scope
        .existing(&root.join("parent/content/shared.md"))
        .is_ok());
    assert!(scope
        .existing(&root.join("sibling/content/secret.md"))
        .is_ok());
    assert!(CallerScope::Human
        .existing(&root.join("parent/content/secret.md"))
        .is_ok());
    assert_eq!(
        CallerScope::Unknown.existing(&root.join("child/content/secret.md")),
        Err(MISSING.into())
    );
    let (space, _) = crate::context_repo::scratch_space::ensure_org_space(
        temp.path(),
        &Principal::from(1u64),
        "acme/ctx",
    )
    .unwrap();
    let private = space.content_dir().join("private.md");
    std::fs::create_dir_all(private.parent().unwrap()).unwrap();
    std::fs::write(&private, "private").unwrap();
    assert!(CallerScope::Unknown.existing(&private).is_ok());
}
