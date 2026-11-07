// SPDX-License-Identifier: MIT
//! Captured local authority and the production effect-time revalidation boundary.
use super::*;
use crate::commands::dashboard_launch::SessionOwner;
use crate::content_engine::ResolverContext;
use crate::domain::SourceRepoSlug;
use std::path::Path;
use tauri::Manager;

#[derive(Clone)]
pub(super) struct SessionFacts {
    pub home: PathBuf,
    pub current_user: u64,
    pub owner: SessionOwner,
    pub workspace: PathBuf,
}

/// These facts come from application-owned session/auth/team state. Tests replace
/// only that state reader; capture, repository resolution and execution are shared.
pub(super) trait LocalFacts: Send + Sync {
    fn session(&self, session_id: &str) -> Result<SessionFacts, String>;
    fn context(&self, owner: &SessionOwner) -> Result<ActiveContext, String>;
}
pub(super) struct AppFacts(pub tauri::AppHandle);
impl LocalFacts for AppFacts {
    fn session(&self, session_id: &str) -> Result<SessionFacts, String> {
        tauri::async_runtime::block_on(super::super::handler::require_live_session(
            &self.0, session_id,
        ))
        .map_err(|error| error.to_string())?;
        let (home, current_user) = tauri::async_runtime::block_on(
            crate::commands::route_to_context::resolve_ambient(&self.0),
        )?;
        let state = self
            .0
            .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
            .ok_or("session registry unavailable")?;
        let owner = state
            .owner_for_session(session_id)
            .ok_or("session has no captured owner")?;
        let workspace = state
            .worktree_dir_for_session(session_id)
            .ok_or("session has no captured workspace")?;
        Ok(SessionFacts {
            home,
            current_user,
            owner,
            workspace,
        })
    }
    fn context(&self, owner: &SessionOwner) -> Result<ActiveContext, String> {
        let team = owner
            .team
            .as_deref()
            .ok_or("session has no captured team")?;
        match tauri::async_runtime::block_on(
            crate::commands::route_to_context::derive_context_for(&self.0, &owner.org, team),
        )? {
            ContextResolution::Ok(ctx) => Ok(ctx),
            _ => Err("captured team unavailable".into()),
        }
    }
}

#[derive(Clone)]
pub(super) struct LocalAdmission {
    pub root: PathBuf,
    pub ctx: ResolverContext,
    slug: SourceRepoSlug,
    owner: SessionOwner,
    session_id: String,
}
impl LocalAdmission {
    pub fn capture(
        facts: &dyn LocalFacts,
        requested_root: &Path,
        session_id: String,
    ) -> Result<Self, String> {
        let root = std::fs::canonicalize(requested_root).map_err(|e| e.to_string())?;
        let current = facts.session(&session_id)?;
        let active = facts.context(&current.owner)?;
        validate_facts(&current, &active, &root)?;
        let ctx = crate::commands::content_engine::derive_resolver_context(
            active.codesense_home,
            active.user_id,
            active.org_slug,
            active.team_name,
        )
        .map_err(|e| e.to_string())?;
        let (ctx, target) = crate::commands::watch::workspace_target(&ctx, &root)?;
        let slug = target.repo.ok_or("workspace has no repository identity")?;
        Ok(Self {
            root,
            ctx,
            slug,
            owner: current.owner,
            session_id,
        })
    }
    pub fn validate(&self, facts: &dyn LocalFacts) -> Result<(), String> {
        let current = facts.session(&self.session_id)?;
        let active = facts.context(&self.owner)?;
        validate_facts(&current, &active, &self.root)?;
        if current.owner != self.owner
            || current.current_user != self.ctx.user_id
            || active.org_slug != self.ctx.org_slug
            || active.team_name != self.ctx.team_name
            || canonical(&active.codesense_home)? != canonical(&self.ctx.codesense_home)?
        {
            return Err("captured session authority changed".into());
        }
        if local_repository(&self.ctx, &self.root).map_err(|e| e.to_string())? != self.slug {
            return Err("captured workspace repository changed".into());
        }
        Ok(())
    }
    /// The workspace's repository, as captured.
    pub fn slug(&self) -> &SourceRepoSlug {
        &self.slug
    }
}
fn canonical(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|e| e.to_string())
}
fn validate_facts(
    current: &SessionFacts,
    active: &ActiveContext,
    root: &Path,
) -> Result<(), String> {
    if current.owner.principal.as_deref()
        != Some(crate::domain::Principal::from(current.current_user).as_str())
        || active.user_id != current.current_user
    {
        return Err("session belongs to another account".into());
    }
    if current.owner.team.is_none()
        || active.org_slug != current.owner.org
        || active.team_name != current.owner.team
        || canonical(&active.codesense_home)? != canonical(&current.home)?
    {
        return Err("session captured team or namespace is unavailable".into());
    }
    if canonical(&current.workspace)? != root {
        return Err("target differs from session workspace".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    struct Fixture {
        _temp: tempfile::TempDir,
        facts: Mutex<SessionFacts>,
        active: ActiveContext,
        unavailable: std::sync::atomic::AtomicBool,
        revoke_after_file: Mutex<Option<PathBuf>>,
        root: PathBuf,
        alias: PathBuf,
        content: PathBuf,
        suppression: crate::content_engine::SuppressionHandle,
    }
    impl LocalFacts for Fixture {
        fn session(&self, _: &str) -> Result<SessionFacts, String> {
            if self.unavailable.load(std::sync::atomic::Ordering::SeqCst)
                || self
                    .revoke_after_file
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|path| path.exists())
            {
                return Err("session is no longer live".into());
            }
            Ok(self.facts.lock().unwrap().clone())
        }
        fn context(&self, _: &SessionOwner) -> Result<ActiveContext, String> {
            Ok(self.active.clone())
        }
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("home");
            let root = temp.path().join("work");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir_all(&root).unwrap();
            let alias = temp.path().join("alias");
            std::os::unix::fs::symlink(&root, &alias).unwrap();
            let active = ActiveContext {
                codesense_home: home.clone(),
                user_id: 42,
                org_slug: "acme/context".into(),
                team_name: Some("team".into()),
            };
            let ctx = crate::commands::content_engine::derive_resolver_context(
                home.clone(),
                42,
                active.org_slug.clone(),
                active.team_name.clone(),
            )
            .unwrap();
            let org = crate::context_repo::path_to_org_root(&home, &ctx.principal(), &ctx.org_slug)
                .unwrap();
            let content = crate::context_repo::path_to_team_content_dir(&org, "team");
            std::fs::create_dir_all(content.join("docs")).unwrap();
            for name in ["a", "b", "c"] {
                std::fs::write(
                    content.join(format!("docs/{name}.md")),
                    format!("saved {name}"),
                )
                .unwrap();
            }
            let store = crate::content_engine::sync_state_store::SyncStateStore::open(
                crate::content_engine::sync_state_store::default_store_path(&home),
            )
            .unwrap();
            let lockfile = crate::content_engine::Lockfile::empty(
                ctx.org_slug.clone(),
                ctx.team_name.clone(),
                root.canonicalize().unwrap(),
            );
            store
                .upsert(42, &SourceRepoSlug::new("acme", "repo").unwrap(), &lockfile)
                .unwrap();
            let owner = SessionOwner {
                principal: Some(ctx.principal().to_string()),
                org: ctx.org_slug,
                team: ctx.team_name,
            };
            let facts = Mutex::new(SessionFacts {
                home,
                current_user: 42,
                owner,
                workspace: root.clone(),
            });
            Self {
                _temp: temp,
                facts,
                active,
                unavailable: false.into(),
                revoke_after_file: Mutex::new(None),
                root,
                alias,
                content,
                suppression: crate::content_engine::dispatcher::Dispatcher::new(
                    0, 100, 60_000, 60_000,
                )
                .suppression_handle(),
            }
        }
        fn capture(&self) -> LocalAdmission {
            LocalAdmission::capture(self, &self.alias, "session".into()).unwrap()
        }
        fn run(
            self: &Arc<Self>,
            admission: &LocalAdmission,
            names: &[&str],
        ) -> Result<Value, String> {
            super::super::explicit::execute_local(
                admission.clone(),
                self.clone(),
                super::super::explicit::Op::Hydrate {
                    paths: names.iter().map(|name| format!("docs/{name}.md")).collect(),
                    sets: Vec::new(),
                    prune: false,
                    overwrite_modified: false,
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .map(|(value, _)| value)
        }
    }

    struct ReRegistered {
        fixture: Fixture,
        app: tauri::AppHandle<tauri::test::MockRuntime>,
    }
    impl LocalFacts for ReRegistered {
        fn session(&self, session: &str) -> Result<SessionFacts, String> {
            let mut facts = self.fixture.session(session)?;
            let launches = self
                .app
                .state::<crate::commands::dashboard_launch::DashboardLaunchState>();
            facts.workspace = launches
                .worktree_dir_for_session(session)
                .ok_or("session has no captured workspace")?;
            facts.owner = launches
                .owner_for_session(session)
                .ok_or("session has no captured owner")?;
            Ok(facts)
        }
        fn context(&self, owner: &SessionOwner) -> Result<ActiveContext, String> {
            self.fixture.context(owner)
        }
    }

    struct RestoredFacts<'a> {
        fixture: &'a Fixture,
        launches: &'a crate::commands::dashboard_launch::DashboardLaunchState,
    }
    impl LocalFacts for RestoredFacts<'_> {
        fn session(&self, session: &str) -> Result<SessionFacts, String> {
            let mut facts = self.fixture.session(session)?;
            facts.workspace = self
                .launches
                .worktree_dir_for_session(session)
                .ok_or("session has no captured workspace")?;
            facts.owner = self
                .launches
                .owner_for_session(session)
                .ok_or("session has no captured owner")?;
            Ok(facts)
        }
        fn context(&self, owner: &SessionOwner) -> Result<ActiveContext, String> {
            self.fixture.context(owner)
        }
    }

    #[test]
    fn local_admission_captures_after_a_restart_restore() {
        use crate::commands::dashboard_launch::{
            DashboardLaunchState, LaunchMeta, LaunchRecord, RestoredLaunch, KIND_DASHBOARD_LAUNCHED,
        };

        let fixture = Fixture::new();
        let path = fixture._temp.path().join("launch-registry.json");
        let before = DashboardLaunchState::new(4);
        before.install_persistence(path.clone());
        before
            .begin_with_provenance(
                "worker",
                None,
                LaunchRecord {
                    meta: LaunchMeta {
                        launched_by: Some("manager".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        before.bind("worker", "session", None).unwrap();
        crate::commands::dashboard_launch::stamp_bound_launch(
            &before,
            "worker",
            false,
            fixture.facts.lock().unwrap().owner.principal.clone(),
            Some("acme/context:team"),
        );
        before.set_worktree_dir_if_absent("worker", fixture.root.clone());

        let after = DashboardLaunchState::new(4);
        after.install_persistence(path);
        let launch = RestoredLaunch {
            session: "session".into(),
            handle: "worker".into(),
            kind: KIND_DASHBOARD_LAUNCHED,
            agent_revision: None,
            team_key: Some("acme/context:team".into()),
            principal: fixture.facts.lock().unwrap().owner.principal.clone(),
            launcher: None,
            bound_at_ms: None,
            started_at_ms: None,
            launched_by: None,
            name: None,
            role: None,
            avatar: None,
            story: None,
            chat_workspace_id: None,
            routine_key: None,
            deadline_ms: None,
        };
        assert!(after.restore_launch(&launch, None, None));

        let facts = RestoredFacts {
            fixture: &fixture,
            launches: &after,
        };
        LocalAdmission::capture(&facts, &fixture.alias, "session".into()).unwrap_or_else(|error| {
            panic!("restored session should capture its workspace: {error}")
        });
    }

    #[test]
    fn local_admission_captures_after_restart_when_workspace_is_stamped_before_bind() {
        use crate::commands::dashboard_launch::{
            DashboardLaunchState, LaunchMeta, LaunchRecord, RestoredLaunch, KIND_DASHBOARD_LAUNCHED,
        };

        let fixture = Fixture::new();
        let path = fixture._temp.path().join("launch-registry.json");
        let before = DashboardLaunchState::new(4);
        before.install_persistence(path.clone());
        before
            .begin_with_provenance(
                "worker",
                None,
                LaunchRecord {
                    meta: LaunchMeta::default(),
                    ..Default::default()
                },
            )
            .unwrap();
        crate::commands::dashboard_launch::stamp_bound_launch(
            &before,
            "worker",
            false,
            fixture.facts.lock().unwrap().owner.principal.clone(),
            Some("acme/context:team"),
        );
        before.set_worktree_dir_if_absent("worker", fixture.root.clone());
        // production order: the workspace is stamped before the (unbounded) bind
        before.bind("worker", "session", None).unwrap();

        let after = DashboardLaunchState::new(4);
        after.install_persistence(path);
        let launch = RestoredLaunch {
            session: "session".into(),
            handle: "worker".into(),
            kind: KIND_DASHBOARD_LAUNCHED,
            agent_revision: None,
            team_key: Some("acme/context:team".into()),
            principal: fixture.facts.lock().unwrap().owner.principal.clone(),
            launcher: None,
            bound_at_ms: None,
            started_at_ms: None,
            launched_by: None,
            name: None,
            role: None,
            avatar: None,
            story: None,
            chat_workspace_id: None,
            routine_key: None,
            deadline_ms: None,
        };
        assert!(after.restore_launch(&launch, None, None));

        let facts = RestoredFacts {
            fixture: &fixture,
            launches: &after,
        };
        LocalAdmission::capture(&facts, &fixture.alias, "session".into()).unwrap_or_else(|error| {
            panic!("restored session should capture its workspace: {error}")
        });
    }

    fn expire_registration_cycle(live: bool, reachable: bool) {
        // Architecture: production ingress also audits cleanup; keep those writes
        // inside an owned HOME for the entire queued-delivery lifetime.
        let _home = crate::routines::test_support::TempHome::new();
        use crate::commands::machines::{
            RuntimeAdmissionState, RuntimeMemberSlot, RuntimeMemberTarget,
        };
        use crate::commands::{
            dashboard_launch::{
                stamp_bound_launch, DashboardLaunchState, LaunchMeta, LaunchRecord,
            },
            pty_host,
        };
        use crate::terminal::session_server::SessionServerState;
        use codesense_pty_core::domain::{
            ClientBootId, PtySessionId, TeardownReason, TerminalDomain, TerminalDomainEvent,
        };
        use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};
        use codesense_session_server::testing::{registration, sh_spec, RecordingSink, TestServer};
        static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        let runtime = RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap());
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let f = Arc::new(ReRegistered {
            fixture: Fixture::new(),
            app: app.handle().clone(),
        });
        let server = TestServer::start();
        let session = format!("capture-{}", uuid::Uuid::new_v4());
        server.track(&session);
        let pty = runtime
            .block_on(LocalPtyRunner.spawn(sh_spec(&session, "sleep 60")))
            .unwrap();
        server
            .host()
            .terminal()
            .register(pty, registration(&session, None, false))
            .unwrap();
        let state = SessionServerState::default();
        let client = state
            .connect_or_launch(server.root(), Arc::new(RecordingSink::default()), |_| {
                Err("no launch".into())
            })
            .unwrap();
        // Refresh through the real session-server layout RPC, as registration does.
        client
            .layout_snapshot(
                &crate::commands::pty_layout::app_viewer(),
                &ClientBootId::new("capture-cycle"),
            )
            .unwrap();
        let row = state
            .admission
            .baseline()
            .rows
            .into_iter()
            .find(|row| row.member.slot == RuntimeMemberSlot::Sidecar)
            .unwrap();
        let RuntimeAdmissionState::Live { namespace } = row.state else {
            panic!("live sidecar")
        };
        let claim = state
            .admission
            .effects
            .reserve(
                RuntimeMemberTarget {
                    member: row.member,
                    namespace,
                    admission_revision: row.revision,
                },
                &session,
            )
            .unwrap();
        let launches = DashboardLaunchState::new(4);
        let persisted = f.fixture._temp.path().join("launches.json");
        launches.install_persistence(persisted.clone());
        launches
            .begin_with_provenance(
                "worker",
                None,
                LaunchRecord {
                    meta: LaunchMeta {
                        launched_by: Some("manager".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        launches
            .bind_owned("worker", &session, None, None, Some(claim.clone()))
            .unwrap();
        stamp_bound_launch(
            &launches,
            "worker",
            false,
            f.fixture.facts.lock().unwrap().owner.principal.clone(),
            Some("acme/context:team"),
        );
        launches.set_worktree_dir_if_absent("worker", f.fixture.root.clone());
        let before = std::fs::read(&persisted).unwrap();
        app.manage(launches);
        app.manage(state);
        if !reachable {
            server.host().close_connections();
            assert!(codesense_session_server::testing::wait_until(
                || !client.is_alive()
            ));
            assert!(
                server
                    .host()
                    .terminal()
                    .alive_session_ids(&[PtySessionId::new(&session)])
                    .unwrap()
                    .contains(&PtySessionId::new(&session)),
                "only connectivity was lost; our test PTY is still live"
            );
        }
        if !live {
            server
                .host()
                .terminal()
                .kill(&PtySessionId::new(&session))
                .unwrap();
        }
        // Drive the real ingress and delivery queue; no recovery helper or fact
        // stamp runs after Expire. The unconnected-server case exercises the
        // synthetic cleanup boundary used by reconnect recovery.
        let event = TerminalDomainEvent::SessionRemoved {
            session: PtySessionId::new(&session),
            reason: TeardownReason::Expire,
        };
        let sink = pty_host::AppTerminalSink::new(
            app.handle().clone(),
            pty_host::StreamHost::LocalSidecar(client.namespace().clone()),
        );
        sink.bind_namespace(client.namespace().clone());
        if reachable {
            codesense_pty_core::domain::TerminalEventSink::emit(&sink, event).unwrap();
            sink.wait_for_expire_checks();
            if !live {
                assert!(codesense_session_server::testing::wait_until(|| app
                    .state::<DashboardLaunchState>()
                    .snapshot()
                    .is_empty()));
            }
        } else {
            pty_host::settle_owned_app_event(app.handle(), &claim, &event, false).unwrap();
        }
        let launches = app.state::<DashboardLaunchState>();
        if live && reachable {
            assert!(
                launches.worktree_dir_for_session(&session).is_some(),
                "a live Expire must retain capture"
            );
            assert_eq!(
                std::fs::read(&persisted).unwrap(),
                before,
                "live Expire must not prune persistence"
            );
            // Registration captures and commits the existing, still-authorizing
            // claim through the same admission gates as dashboard registration.
            let next = app
                .state::<SessionServerState>()
                .admission
                .effects
                .reserve(claim.target.clone(), &session)
                .unwrap();
            crate::commands::pty_backend::commit_local_effect_claim(app.handle(), &next, || {
                launches.register_owned("worker", &session, None, Some(next.clone()))
            })
            .unwrap();
            let admission = LocalAdmission::capture(f.as_ref(), &f.fixture.alias, session).unwrap();
            std::fs::create_dir_all(f.fixture.root.join("docs")).unwrap();
            std::fs::write(f.fixture.root.join("docs/new.md"), "retained session work").unwrap();
            super::super::explicit::execute_local(
                admission,
                f.clone(),
                super::super::explicit::Op::Publish {
                    paths: vec!["docs/new.md".into()],
                    sets: vec![],
                    to: None,
                    include_deletes: false,
                    expect: Default::default(),
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
            assert_eq!(
                std::fs::read_to_string(f.fixture.content.join("docs/new.md")).unwrap(),
                "retained session work"
            );
        } else {
            assert!(launches.snapshot().is_empty());
            let record: Value =
                serde_json::from_slice(&std::fs::read(&persisted).unwrap()).unwrap();
            assert!(
                record["launches"].get("worker").is_none(),
                "terminal/unreachable release must prune"
            );
            launches
                .register_owned("worker", &session, None, Some(claim))
                .unwrap();
            assert!(launches.worktree_dir_for_session(&session).is_none());
            assert!(launches.owner_for_session(&session).is_none());
            assert!(launches.launcher_context(&session).is_none());
            assert!(launches.snapshot()[0].meta.launched_by.is_none());
        }
    }
    #[test]
    fn live_expire_keeps_capture_then_registration_admission_and_publish_work() {
        expire_registration_cycle(true, true);
    }
    #[test]
    fn ended_expire_releases_and_prunes_without_resurrecting_facts() {
        expire_registration_cycle(false, true);
    }
    #[test]
    fn unreachable_expire_keeps_existing_release_and_prune_behavior() {
        expire_registration_cycle(true, false);
    }
    /// A local target is admitted through its live session and
    /// canonical root, then hydrated on the one engine.
    #[test]
    fn public_local_admission_canonicalizes_alias_and_hydrates_on_the_engine() {
        let f = Arc::new(Fixture::new());
        let admission = f.capture();
        assert_eq!(admission.root, f.root.canonicalize().unwrap());
        assert_ne!(admission.root, f.alias);
        let value = f.run(&admission, &["a"]).unwrap();
        assert_eq!(value["status"], "ready", "{value}");
        assert_eq!(
            std::fs::read(f.alias.join("docs/a.md")).unwrap(),
            b"saved a"
        );
        assert_eq!(f.run(&admission, &["b"]).unwrap()["status"], "ready");
        assert_eq!(
            std::fs::read(f.alias.join("docs/b.md")).unwrap(),
            b"saved b"
        );
        let _ = &f.content;
    }
    /// "Discard my edits" on a real workspace writes
    /// exactly the accepted files — an edited file not named keeps its edit,
    /// a folder is refused before anything is written, and a file the team
    /// no longer has is reported, not touched.
    #[test]
    fn xs10_discard_writes_only_the_accepted_files() {
        let f = Arc::new(Fixture::new());
        let admission = f.capture();
        assert_eq!(
            f.run(&admission, &["a", "b", "c"]).unwrap()["status"],
            "ready"
        );
        for name in ["a", "b", "c"] {
            std::fs::write(
                f.alias.join(format!("docs/{name}.md")),
                format!("edited {name}"),
            )
            .unwrap();
        }
        std::fs::remove_file(f.content.join("docs/c.md")).unwrap();
        let discard = |paths: &[&str]| {
            super::super::explicit::execute_local(
                admission.clone(),
                f.clone(),
                super::super::explicit::Op::Discard {
                    paths: paths.iter().map(|p| p.to_string()).collect(),
                    expect: Default::default(),
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .map(|(value, _)| value)
        };
        // A folder (and a padded path) is refused; nothing is written.
        assert!(discard(&["docs"]).is_err());
        assert!(discard(&[" docs/a.md"]).is_err());
        assert_eq!(
            std::fs::read(f.alias.join("docs/a.md")).unwrap(),
            b"edited a"
        );
        let value = discard(&["docs/a.md", "docs/c.md"]).unwrap();
        assert_eq!(value["written"], 1, "{value}");
        assert_eq!(value["notInTeam"], json!(["docs/c.md"]), "{value}");
        assert_eq!(
            std::fs::read(f.alias.join("docs/a.md")).unwrap(),
            b"saved a"
        );
        // Not named: keeps its edit. Gone from the team: left as it is.
        assert_eq!(
            std::fs::read(f.alias.join("docs/b.md")).unwrap(),
            b"edited b"
        );
        assert_eq!(
            std::fs::read(f.alias.join("docs/c.md")).unwrap(),
            b"edited c"
        );
    }

    /// Discard of a NEW file (never copied here) deletes it — only
    /// a new file under the workspace's team folders; anything else is
    /// refused before anything is written. A copied file in the same call
    /// gets the team's version back.
    #[test]
    fn xs27_discard_deletes_a_new_file_and_restores_a_copied_one() {
        let f = Arc::new(Fixture::new());
        let admission = f.capture();
        let op = |op| {
            super::super::explicit::execute_local(
                admission.clone(),
                f.clone(),
                op,
                &uuid::Uuid::new_v4().to_string(),
            )
            .map(|(value, _)| value)
        };
        let hydrated = op(super::super::explicit::Op::Hydrate {
            paths: Vec::new(),
            sets: vec!["docs".into()],
            prune: false,
            overwrite_modified: false,
        })
        .unwrap();
        assert_eq!(hydrated["status"], "ready", "{hydrated}");
        std::fs::write(f.alias.join("docs/a.md"), b"edited a").unwrap();
        std::fs::write(f.alias.join("docs/new.md"), b"a new note").unwrap();
        std::fs::write(f.alias.join("outside.md"), b"not team content").unwrap();
        let discard = |paths: &[&str]| {
            op(super::super::explicit::Op::Discard {
                paths: paths.iter().map(|p| p.to_string()).collect(),
                expect: Default::default(),
            })
        };
        // Not a new file under the team folders: refused, nothing touched.
        assert!(discard(&["outside.md", "docs/new.md"]).is_err());
        assert!(f.alias.join("outside.md").exists());
        assert!(f.alias.join("docs/new.md").exists());
        let value = discard(&["docs/new.md", "docs/a.md"]).unwrap();
        assert_eq!(value["removedNew"], json!(["docs/new.md"]), "{value}");
        assert!(!f.alias.join("docs/new.md").exists());
        assert_eq!(
            std::fs::read(f.alias.join("docs/a.md")).unwrap(),
            b"saved a"
        );
        // Team content is never touched by a discard.
        assert!(!f.content.join("docs/new.md").exists());
        // A folder is never a discard of the new files in it.
        std::fs::create_dir_all(f.alias.join("docs/more")).unwrap();
        std::fs::write(f.alias.join("docs/more/n.md"), b"new").unwrap();
        assert!(discard(&["docs/more"]).is_err());
        assert!(f.alias.join("docs/more/n.md").exists());
        // A refused new path refuses the whole call: the copied file in it
        // keeps its edit.
        std::fs::write(f.alias.join("docs/a.md"), b"edited again").unwrap();
        assert!(discard(&["docs/a.md", "outside.md"]).is_err());
        assert_eq!(
            std::fs::read(f.alias.join("docs/a.md")).unwrap(),
            b"edited again"
        );
    }

    #[test]
    fn captured_owner_and_effect_time_session_changes_are_rejected() {
        let f = Fixture::new();
        f.facts.lock().unwrap().owner.principal = Some("github:99".into());
        assert!(LocalAdmission::capture(&f, &f.alias, "session".into()).is_err());
        let f = Arc::new(Fixture::new());
        let admission = f.capture();
        // The session ends after admission: nothing is written.
        f.unavailable
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(f.run(&admission, &["a", "b"]).is_err());
        assert!(!f.root.join("docs/a.md").exists());
        assert!(!f.root.join("docs/b.md").exists());
        f.unavailable
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(f.run(&admission, &["c"]).unwrap()["status"], "ready");
        // An account change after admission is refused the same way.
        f.facts.lock().unwrap().current_user = 99;
        assert!(f.run(&admission, &["a"]).is_err());
        assert!(!f.root.join("docs/a.md").exists());
        let _ = &f.revoke_after_file;
        let _ = &f.suppression;
    }

    /// Every mutating operation ends with exactly one terminal
    /// progress event carrying its operationId — on success, on an error
    /// before the engine runs, and on an error at the effect-time check.
    #[test]
    fn every_operation_ends_with_exactly_one_terminal_event() {
        use crate::agents::prepare::PreparePhase;
        let f = Arc::new(Fixture::new());
        let admission = f.capture();
        let terminal = |events: &[crate::agents::prepare::PrepareProgress], id: &str| {
            let ends: Vec<_> = events
                .iter()
                .filter(|e| {
                    matches!(
                        e.phase,
                        PreparePhase::Ready | PreparePhase::Failed | PreparePhase::Cancelled
                    )
                })
                .collect();
            assert_eq!(ends.len(), 1, "{events:?}");
            assert!(events.iter().all(|e| e.operation_id.as_deref() == Some(id)));
            ends[0].phase
        };
        let hydrate = |paths: Vec<String>| super::super::explicit::Op::Hydrate {
            paths,
            sets: Vec::new(),
            prune: false,
            overwrite_modified: false,
        };
        let id = uuid::Uuid::new_v4().to_string();
        let (ok, events) = super::super::explicit::execute_local_events(
            admission.clone(),
            f.clone(),
            hydrate(vec!["docs/a.md".into()]),
            &id,
        );
        assert!(ok.is_ok());
        assert_eq!(terminal(&events, &id), PreparePhase::Ready);
        // An invalid selection fails before the engine runs.
        let id = uuid::Uuid::new_v4().to_string();
        let (err, events) = super::super::explicit::execute_local_events(
            admission.clone(),
            f.clone(),
            hydrate(vec!["../escape.md".into()]),
            &id,
        );
        assert!(err.is_err());
        assert_eq!(terminal(&events, &id), PreparePhase::Failed);
        // The session ends: the admission check fails.
        f.unavailable
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let id = uuid::Uuid::new_v4().to_string();
        let (err, events) = super::super::explicit::execute_local_events(
            admission,
            f.clone(),
            hydrate(vec!["docs/b.md".into()]),
            &id,
        );
        assert!(err.is_err());
        assert_eq!(terminal(&events, &id), PreparePhase::Failed);
    }
}
