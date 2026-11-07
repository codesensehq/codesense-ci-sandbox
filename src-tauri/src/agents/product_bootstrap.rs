// SPDX-License-Identifier: MIT
//! Explicit selected startup for durable product and story roots, before a session exists.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{launch::AgentLaunch, ManifestTarget};
use crate::content_engine::{
    context_lifecycle,
    sync_state_store::{default_store_path, SyncStateStore},
    ResolverContext,
};
use crate::domain::{Principal, SourceRepoSlug};

#[derive(Debug, Clone)]
pub struct LaunchContext {
    pub home: PathBuf,
    pub principal: Principal,
    pub target: ManifestTarget,
}

#[derive(Debug)]
pub(crate) enum BootstrapOutcome {
    NotApplicable,
    Context {
        home: PathBuf,
        record: crate::install::worktree_index::ContextWorktreeRecord,
    },
    Prepared(Box<ProductAuthority>),
}

#[derive(Debug)]
pub(crate) struct ProductAuthority {
    requested: PathBuf,
    root: PathBuf,
    slug: SourceRepoSlug,
    ctx: ResolverContext,
    story: Option<crate::install::worktree_index::WorktreeStoryRecord>,
    /// What Step 2 did, once it ran (SW-XS-7).
    report: Option<super::prepare::PrepareReport>,
}

impl ProductAuthority {
    fn capture(launch: &AgentLaunch, cwd: &Path) -> Result<Option<Self>, String> {
        let captured = &launch.startup_context;
        let store =
            SyncStateStore::open(default_store_path(&captured.home)).map_err(|e| e.to_string())?;
        let stories = crate::install::worktree_index::story_registrations(&captured.home, cwd)
            .map_err(|e| e.to_string())?;
        let (user_id, org, team, slug, story) = if !stories.is_empty() {
            let mut owned = stories.into_iter().filter(|(principal, record)| {
                principal == &captured.principal
                    && record.org_slug == captured.target.org
                    && crate::install::worktree_index::resolve_team(record).as_deref()
                        == Some(captured.target.team.as_str())
            });
            let (_, record) = owned
                .next()
                .ok_or("story ownership differs from captured principal/team")?;
            if owned.next().is_some() {
                return Err("story workspace identity is ambiguous".into());
            }
            if !record.setup_complete {
                return Err("story workspace setup is incomplete".into());
            }
            (
                captured.principal.numeric_id()?,
                record.org_slug.clone(),
                Some(captured.target.team.clone()),
                record.source_repo.clone(),
                Some(record),
            )
        } else if let Some(registration) = store
            .product_registration_for(
                cwd,
                captured.principal.numeric_id()?,
                &captured.target.org,
                Some(&captured.target.team),
            )
            .map_err(|e| e.to_string())?
        {
            (
                registration.user_id,
                registration.org_slug,
                registration.team_name,
                registration.slug,
                None,
            )
        } else {
            // A managed Git worktree awaiting its story record cannot fall through
            // to the ordinary-directory launch path.
            if store
                .uses_explicit_context(&context_lifecycle::local_identity(cwd))
                .map_err(|e| e.to_string())?
                || (cwd.join(".git").is_file()
                    && crate::install::worktree_index::is_codesense_managed_target_canon(
                        cwd,
                        &captured.home,
                    ))
            {
                return Err("managed story workspace registration is missing".into());
            }
            return Ok(None);
        };
        let authority = Self {
            requested: cwd.to_path_buf(),
            root: cwd
                .canonicalize()
                .map_err(|e| format!("workspace root unavailable: {e}"))?,
            slug,
            ctx: crate::commands::content_engine::derive_resolver_context(
                captured.home.clone(),
                user_id,
                org,
                team,
            )
            .map_err(|e| e.to_string())?,
            story,
            report: None,
        };
        authority.validate()?;
        Ok(Some(authority))
    }

    fn validate(&self) -> Result<(), String> {
        if self.requested.canonicalize().map_err(|e| e.to_string())? != self.root
            || !self.root.is_dir()
        {
            return Err("product root changed during startup preparation".into());
        }
        if let Some(captured) = &self.story {
            let current = crate::install::worktree_index::read_record(
                &self.ctx.codesense_home,
                &self.ctx.principal(),
                &captured.code_repo_path,
            )
            .map_err(|e| e.to_string())?
            .ok_or("story workspace registration disappeared")?;
            if current.org_slug != self.ctx.org_slug
                || current.source_repo != self.slug
                || current.story_id != captured.story_id
                || current.story_source_path != captured.story_source_path
                || crate::install::worktree_index::resolve_team(&current) != self.ctx.team_name
                || current
                    .code_repo_path
                    .canonicalize()
                    .map_err(|e| e.to_string())?
                    != self.root
                || !current.setup_complete
            {
                return Err(
                    "story workspace ownership or setup changed during startup preparation".into(),
                );
            }
        } else {
            let registration = SyncStateStore::open(default_store_path(&self.ctx.codesense_home))
                .map_err(|e| e.to_string())?
                .product_registration_for(
                    &self.root,
                    self.ctx.user_id,
                    &self.ctx.org_slug,
                    self.ctx.team_name.as_deref(),
                )
                .map_err(|e| e.to_string())?;
            if registration.as_ref().map(|r| &r.slug) != Some(&self.slug) {
                return Err(
                    "product workspace ownership changed during startup preparation".into(),
                );
            }
        }
        let source = crate::context_repo::path_to_org_root(
            &self.ctx.codesense_home,
            &self.ctx.principal(),
            &self.ctx.org_slug,
        )?;
        let mut sources = vec![source];
        let target = ManifestTarget {
            context_root: None,
            org: self.ctx.org_slug.clone(),
            team: self.ctx.team_name.clone().unwrap_or_default(),
            repo: None,
        };
        sources.extend(
            super::manifest_resolve::content_layers(
                &self.ctx.codesense_home,
                &self.ctx.principal(),
                &target,
            )
            .into_iter()
            .map(|(_, root)| root.safety_boundary()),
        );
        for source in sources {
            if let Ok(source) = source.canonicalize() {
                if self.root.starts_with(&source) || source.starts_with(&self.root) {
                    return Err("product startup destination overlaps a context source root".into());
                }
            }
        }
        Ok(())
    }
}

/// Step 2 for a durable product or story root (SW-XS-7): the agent's whole
/// want set — its captured startup manifest pinned first, then its context —
/// through the one preparation engine. Unmodified stale managed files are
/// replaced; edited, unmanaged and tracked files are kept and reported. Any
/// failed entry fails the launch (§11.7).
fn receive(
    authority: &ProductAuthority,
    launch: &AgentLaunch,
    progress: &dyn Fn(&super::prepare::PrepareProgress),
) -> Result<super::prepare::PrepareReport, String> {
    use super::prepare;
    authority.validate()?;
    if !launch.manifest.errors.is_empty() {
        return Err(format!(
            "startup manifest resolution failed: {}",
            serde_json::to_string(&launch.manifest.errors).map_err(|e| e.to_string())?
        ));
    }
    let ctx = &authority.ctx;
    let principal = ctx.principal();
    let target = ManifestTarget {
        repo: Some(authority.slug.clone()),
        ..launch.startup_context.target.clone()
    };
    let mut want = prepare::WantSet::for_agent(
        &ctx.codesense_home,
        &principal,
        &launch.manifest.agent.key,
        &target,
        false,
        Some(&launch.manifest),
    )?;
    // SW-XS-8: merged workspace files ride Step 2 itself (managed, excluded
    // per workspace, under the destination's lock); Step 2 folds the interim
    // record XS-14 kept for them under that lock.
    want.add_workspace_files(&ctx.codesense_home, &principal, &launch.generated)?;
    let legacy = || prepare::legacy_rows_local(ctx, &authority.root);
    let report = prepare::prepare(prepare::PrepareRequest {
        home: &ctx.codesense_home,
        principal: &principal,
        destination: prepare::Destination::Local {
            root: &authority.root,
        },
        want: &want,
        origin: format!("step2:{}", launch.manifest.agent.key),
        workspace: authority.root.display().to_string(),
        agent: launch.name.clone(),
        legacy: &legacy,
        prune: false,
        progress,
        validate: &|| authority.validate(),
        explicit: Default::default(),
    })?;
    let workspace = authority.root.display().to_string();
    let finish = |report: &prepare::PrepareReport| {
        prepare::progress::finish(progress, report, &workspace, &launch.name, "team content")
    };
    if let Some(failure) = report.failure() {
        finish(&report);
        return Err(failure);
    }
    let post = (|| {
        authority.validate()?;
        for essential in &launch.manifest.essentials {
            match essential.id.as_str() {
                // Per-session projection applies these essentials fallibly before spawn.
                "codesense-bridge" | "codesense-notify-settings" => {}
                id => return Err(format!("unsupported startup essential: {id}")),
            }
        }
        Ok(())
    })();
    if let Err(error) = post {
        let mut failed = report.clone();
        failed.status = prepare::PrepareStatus::Failed;
        failed.failed.push(("launch".into(), error.clone()));
        finish(&failed);
        return Err(error);
    }
    // `ready` only now: Step 2 and its post-checks are done (X5).
    finish(&report);
    log::info!(target: "agents", "{}", report.startup_note("team content"));
    Ok(report)
}

impl BootstrapOutcome {
    /// What Step 2 did for this launch: the structured MCP / UI result.
    pub(crate) fn report(&self) -> Option<&super::prepare::PrepareReport> {
        match self {
            Self::NotApplicable | Self::Context { .. } => None,
            Self::Prepared(authority) => authority.report.as_ref(),
        }
    }
    pub(crate) fn canonical_root(&self) -> Option<&Path> {
        match self {
            Self::NotApplicable => None,
            Self::Context { record, .. } => Some(&record.launch_cwd),
            Self::Prepared(authority) => Some(&authority.root),
        }
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        match self {
            Self::NotApplicable => Ok(()),
            Self::Context { home, record } => {
                let current =
                    crate::install::worktree_index::context_for_cwd(home, &record.launch_cwd)
                        .ok_or("context worktree registration disappeared")?;
                if current.id != record.id || current.root != record.root {
                    return Err("context worktree authority changed".into());
                }
                crate::commands::workspace_open::validate_context_worktree(record)
            }
            Self::Prepared(authority) => authority.validate(),
        }
        .map_err(|e| format!("product startup preparation failed: {e}"))
    }
}

/// Where a launch's Step 2 reports progress.
pub(crate) struct BootstrapResources {
    pub sink: Arc<dyn crate::commands::worktree::EventSink>,
}

pub(crate) fn resources(app: &tauri::AppHandle) -> Option<BootstrapResources> {
    Some(BootstrapResources {
        sink: Arc::new(crate::commands::worktree::TauriEventSink(app.clone())),
    })
}

/// Shared prelaunch seam: Step 2 under its own destination-scoped lock — never
/// the wave coordinator's repo lock (SW-XS-7, §11.7); a failure is propagated
/// before scoped config or process spawn.
pub(crate) async fn prepare(
    app: &tauri::AppHandle,
    launch: &AgentLaunch,
    cwd: &Path,
) -> Result<BootstrapOutcome, String> {
    prepare_with_resources(launch, cwd, resources(app)).await
}

pub(crate) async fn prepare_with_resources(
    launch: &AgentLaunch,
    cwd: &Path,
    resources: Option<BootstrapResources>,
) -> Result<BootstrapOutcome, String> {
    let launch = launch.clone();
    let path = cwd.to_path_buf();
    if let Some(record) =
        crate::install::worktree_index::context_for_cwd(&launch.startup_context.home, cwd)
    {
        if record.org != launch.startup_context.target.org
            || record.team != launch.startup_context.target.team
        {
            return Err("context worktree differs from captured team".into());
        }
        let outcome = BootstrapOutcome::Context {
            home: launch.startup_context.home.clone(),
            record,
        };
        outcome.validate()?;
        return Ok(outcome);
    }
    let capture_launch = launch.clone();
    let authority =
        crate::commands::async_helpers::await_blocking("product startup capture", move || {
            ProductAuthority::capture(&capture_launch, &path)
        })
        .await
        .map_err(|e| format!("product startup preparation failed: {e}"))?;
    let Some(authority) = authority else {
        return Ok(BootstrapOutcome::NotApplicable);
    };
    let BootstrapResources { sink } =
        resources.ok_or("product startup preparation failed: progress sink unavailable")?;
    let authority =
        crate::commands::async_helpers::await_blocking("product startup receive", move || {
            let mut authority = authority;
            authority.report = Some(receive(
                &authority,
                &launch,
                &super::prepare::progress::to_sink(sink.as_ref()),
            )?);
            Ok(authority)
        })
        .await
        .map_err(|e| format!("product startup preparation failed: {e}"))?;
    Ok(BootstrapOutcome::Prepared(Box::new(authority)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::agents::launch;
    use crate::agents::resolve::tests::{alice, draft, save, team_content, write, ORG, TEAM};
    use codesense_pty_core::agent_cli::AgentCli;

    pub(crate) fn fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        AgentLaunch,
        ProductAuthority,
    ) {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        team_content(home.path(), &alice());
        let key = save(
            home.path(),
            &alice(),
            draft("product-worker", AgentCli::Claude),
        );
        let launch = launch::prepare(
            home.path(),
            &alice(),
            &key,
            launch::PrepareOptions {
                target_team: Some((ORG.into(), TEAM.into())),
                ..Default::default()
            },
        )
        .unwrap();
        let ctx = crate::commands::content_engine::derive_resolver_context(
            home.path().into(),
            1,
            ORG.into(),
            Some(TEAM.into()),
        )
        .unwrap();
        let slug = crate::domain::derive_epic_workspace_slug("arbitrary-root-name").unwrap();
        crate::content_engine::register_attached_code_repo(&ctx, &slug, root.path()).unwrap();
        write(
            root.path(),
            ".mcp.json",
            br#"{"mcpServers":{"codesense":{"command":"codesense-bridge"}}}"#,
        );
        let authority = ProductAuthority::capture(&launch, root.path())
            .unwrap()
            .unwrap();
        (home, root, launch, authority)
    }

    fn story_fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        AgentLaunch,
        ProductAuthority,
    ) {
        let (home, root, launch, product) = fixture();
        let store = SyncStateStore::open(default_store_path(home.path())).unwrap();
        store.delete_all_for_path(root.path()).unwrap();
        let source = crate::agents::resolve::tests::content(home.path(), &alice(), TEAM)
            .join("stories/S-1.md");
        let record = crate::install::worktree_index::WorktreeStoryRecord::new(
            root.path(),
            ORG,
            SourceRepoSlug::new("acme", "repo").unwrap(),
            "S-1",
            source,
        );
        crate::install::worktree_index::write_record(home.path(), &alice(), &record).unwrap();
        drop(product);
        let authority = ProductAuthority::capture(&launch, root.path())
            .unwrap()
            .unwrap();
        assert!(authority.story.is_some());
        (home, root, launch, authority)
    }

    #[tokio::test]
    async fn explicit_story_pending_setup_failure_and_retry_gate_native_launch() {
        let (home, root, launch, authority) = story_fixture();
        let mut record = authority.story.unwrap();
        record.setup_complete = false;
        crate::install::worktree_index::write_record(home.path(), &alice(), &record).unwrap();
        assert!(prepare_with_resources(&launch, root.path(), None)
            .await
            .unwrap_err()
            .contains("setup is incomplete"));
        let setup_authority = crate::commands::worktree::WorktreeConfigAuthority {
            codesense_home: home.path().into(),
            principal: alice(),
            team: TEAM.into(),
        };
        let registry = crate::commands::worktree::WorktreeRunRegistry::new();
        let org_root = crate::context_repo::path_to_org_root(home.path(), &alice(), ORG).unwrap();
        let config = crate::commands::worktree::WorktreeConfig {
            setup: vec!["exit 1".into()],
            ..Default::default()
        };
        let failed = crate::commands::worktree::run_setup_for_worktree(
            &crate::commands::worktree::NullEventSink,
            &registry,
            root.path(),
            root.path(),
            &org_root,
            &setup_authority,
            config,
            None,
            "failed-setup",
        )
        .await
        .unwrap();
        assert_ne!(failed.exit_code, 0);
        // Reopen/registration retains the pending state and does not hydrate.
        assert!(!crate::commands::content_engine::register_story_workspace(
            home.path(),
            &alice(),
            record
        )
        .unwrap());
        assert!(prepare_with_resources(&launch, root.path(), None)
            .await
            .is_err());
        crate::commands::worktree::run_setup_for_worktree(
            &crate::commands::worktree::NullEventSink,
            &registry,
            root.path(),
            root.path(),
            &org_root,
            &setup_authority,
            crate::commands::worktree::WorktreeConfig::default(),
            None,
            "retry-no-steps",
        )
        .await
        .unwrap();
        let prepared = prepare_with_resources(
            &launch,
            root.path(),
            Some(BootstrapResources {
                sink: Arc::new(crate::commands::worktree::NullEventSink),
            }),
        )
        .await
        .unwrap();
        prepared.validate().unwrap();
        assert_eq!(
            prepared.canonical_root(),
            Some(root.path().canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn explicit_story_startup_prepares_manifest_and_preserves_unrelated_edits() {
        let (_home, root, launch, authority) = story_fixture();
        write(root.path(), "existing.md", b"edited context");
        receive(&authority, &launch, &|_| {}).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("CLAUDE.md")).unwrap(),
            b"# Team\n"
        );
        assert_eq!(
            std::fs::read(root.path().join("existing.md")).unwrap(),
            b"edited context"
        );
        assert!(!root.path().join(".agents/skills/review/SKILL.md").exists());
    }

    /// A Claude launch writes no status hook into its workspace: the hook runs from
    /// CodeSense's host.
    #[test]
    fn launch_writes_no_status_hook() {
        let (_home, root, launch, authority) = story_fixture();
        receive(&authority, &launch, &|_| {}).unwrap();
        assert!(!root
            .path()
            .join(".claude/hooks")
            .join(codesense_pty_core::notify_hook::NAME)
            .exists());
    }

    /// SW-XS-7 §11.7: Step 2 that fails an entry blocks the spawn
    /// continuation; a user's own differing file is kept (§11.1), never a
    /// failure and never overwritten.
    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_story_startup_failure_blocks_spawn_continuation() {
        use std::os::unix::fs::PermissionsExt;
        let (_home, root, launch, _authority) = story_fixture();
        write(root.path(), "CLAUDE.md", b"workspace edits");
        std::fs::create_dir_all(root.path().join(".claude")).unwrap();
        std::fs::set_permissions(
            root.path().join(".claude"),
            std::fs::Permissions::from_mode(0o555),
        )
        .unwrap();
        let resources = BootstrapResources {
            sink: Arc::new(crate::commands::worktree::NullEventSink),
        };
        let mut spawned = false;
        let result = prepare_with_resources(&launch, root.path(), Some(resources))
            .await
            .map(|_| {
                spawned = true;
            });
        std::fs::set_permissions(
            root.path().join(".claude"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(result.unwrap_err().contains("Couldn't prepare"));
        assert!(!spawned);
        assert_eq!(
            std::fs::read(root.path().join("CLAUDE.md")).unwrap(),
            b"workspace edits"
        );
    }

    /// SW-XS-7 §11.7: the local spawn path no longer takes the wave
    /// coordinator's repo lock: preparation completes while it is held.
    #[tokio::test]
    async fn local_step_two_does_not_wait_on_the_wave_repo_lock() {
        let (_home, root, launch, _authority) = story_fixture();
        let coordinator = crate::content_engine::sync::wave::WaveCoordinator::new();
        let _held = coordinator.acquire_repo_lock(root.path()).await;
        let prepared = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            prepare_with_resources(
                &launch,
                root.path(),
                Some(BootstrapResources {
                    sink: Arc::new(crate::commands::worktree::NullEventSink),
                }),
            ),
        )
        .await
        .expect("Step 2 never waits on the wave coordinator's repo lock")
        .unwrap();
        prepared.validate().unwrap();
        assert_eq!(
            std::fs::read(root.path().join("CLAUDE.md")).unwrap(),
            b"# Team\n"
        );
    }

    #[test]
    fn explicit_story_revalidates_owner_source_and_setup_before_effects() {
        let (home, root, mut launch, authority) = story_fixture();
        launch.startup_context.principal = Principal::from(2);
        assert!(ProductAuthority::capture(&launch, root.path()).is_err());
        let mut record = authority.story.clone().unwrap();
        record.setup_complete = false;
        crate::install::worktree_index::write_record(home.path(), &alice(), &record).unwrap();
        assert!(authority.validate().is_err());
        record.setup_complete = true;
        record.story_source_path = home.path().join("_team/other/S-1.md");
        crate::install::worktree_index::write_record(home.path(), &alice(), &record).unwrap();
        assert!(authority.validate().is_err());
        crate::install::worktree_index::remove_record(home.path(), &alice(), root.path()).unwrap();
        assert!(authority.validate().is_err());
    }

    #[test]
    fn shared_product_selects_captured_team_and_rejects_ambiguity_within_it() {
        let (_home, root, launch, authority) = fixture();
        let mut other = authority.ctx.clone();
        other.team_name = Some("platform".into());
        crate::content_engine::register_attached_code_repo(&other, &authority.slug, root.path())
            .unwrap();
        authority.validate().unwrap();
        let mut other_launch = launch.clone();
        other_launch.startup_context.target.team = "platform".into();
        ProductAuthority::capture(&other_launch, root.path())
            .unwrap()
            .unwrap()
            .validate()
            .unwrap();
        let conflicting = crate::domain::derive_epic_workspace_slug("different").unwrap();
        crate::content_engine::register_attached_code_repo(&other, &conflicting, root.path())
            .unwrap();
        authority.validate().unwrap();
        assert!(ProductAuthority::capture(&other_launch, root.path()).is_err());
    }

    #[test]
    fn product_projection_keeps_bridge_and_notify_private_and_fails_before_ready() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let (_home, root, launch, authority) = fixture();
        std::fs::remove_file(root.path().join(".mcp.json")).unwrap();
        receive(&authority, &launch, &|_| {}).unwrap();
        let session = format!("product-projection-{}", uuid::Uuid::new_v4());
        let (scope, _, applied) =
            launch::project_and_apply(&launch, &session, root.path(), None, str::to_string)
                .unwrap();
        assert!(scope
            .extra_args
            .contains(&"--strict-mcp-config".to_string()));
        let settings = applied.settings.as_ref().unwrap();
        assert!(!settings.path().starts_with(root.path()));
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(settings.path()).unwrap()).unwrap();
        assert!(body.get("hooks").is_some());
        assert!(!root.path().join(".mcp.json").exists());
        assert!(!root.path().join(".claude/settings.local.json").exists());
        crate::commands::mcp_scope::remove_scope_config(&session);
        write(root.path(), ".claude/settings.local.json", b"malformed");
        let refused = format!("product-projection-{}", uuid::Uuid::new_v4());
        assert!(
            launch::project_and_apply(&launch, &refused, root.path(), None, str::to_string,)
                .unwrap_err()
                .contains("notification settings")
        );
        assert!(!crate::commands::mcp_scope::mcp_scope_session_dir(&refused).exists());
    }

    #[test]
    fn required_notify_projection_refuses_invalid_settings_without_folder_writes() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = serde_json::json!({});
        crate::commands::content_engine::merge_required_launch_notify_hooks(
            &mut settings,
            root.path(),
            codesense_pty_core::agent_cli::AgentCli::Claude,
        )
        .unwrap();
        assert!(settings.get("hooks").is_some());
        assert!(!root.path().join(".claude").exists());
        assert!(!root.path().join(".mcp.json").exists());
        for bytes in [b"malformed".as_slice(), b"[]".as_slice()] {
            write(root.path(), ".claude/settings.local.json", bytes);
            assert!(
                crate::commands::content_engine::merge_required_launch_notify_hooks(
                    &mut serde_json::json!({}),
                    root.path(),
                    codesense_pty_core::agent_cli::AgentCli::Claude,
                )
                .unwrap_err()
                .contains("notification settings")
            );
            assert_eq!(
                std::fs::read(root.path().join(".claude/settings.local.json")).unwrap(),
                bytes
            );
        }
        let oversized = root.path().join(".claude/settings.local.json");
        std::fs::File::create(&oversized)
            .unwrap()
            .set_len(1024 * 1024 + 1)
            .unwrap();
        assert!(
            crate::commands::content_engine::merge_required_launch_notify_hooks(
                &mut serde_json::json!({}),
                root.path(),
                codesense_pty_core::agent_cli::AgentCli::Claude,
            )
            .unwrap_err()
            .contains("exceed 1 MiB")
        );
        #[cfg(unix)]
        {
            std::fs::remove_file(&oversized).unwrap();
            std::os::unix::fs::symlink("/dev/zero", &oversized).unwrap();
            assert!(
                crate::commands::content_engine::merge_required_launch_notify_hooks(
                    &mut serde_json::json!({}),
                    root.path(),
                    codesense_pty_core::agent_cli::AgentCli::Claude,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn registered_product_prepares_exact_manifest_and_retained_retry_preserves_edits() {
        let (_home, root, launch, authority) = fixture();
        write(root.path(), "old-hydrated.md", b"user edits");
        receive(&authority, &launch, &|_| {}).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("CLAUDE.md")).unwrap(),
            b"# Team\n"
        );
        assert_eq!(
            std::fs::read(root.path().join("old-hydrated.md")).unwrap(),
            b"user edits"
        );
        assert!(!root.path().join(".agents/skills/review/SKILL.md").exists());
        write(root.path(), "CLAUDE.md", b"workspace edits");
        receive(&authority, &launch.clone(), &|_| {}).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("CLAUDE.md")).unwrap(),
            b"workspace edits"
        );
    }

    /// §11.1: a file the user already has at a wanted path is kept
    /// (unmanaged) and reported; a wrong principal still blocks startup.
    #[test]
    fn unmanaged_workspace_file_is_kept_and_wrong_principal_blocks_startup() {
        let (_home, root, mut launch, authority) = fixture();
        write(root.path(), "CLAUDE.md", b"existing user edits");
        let report = receive(&authority, &launch, &|_| {}).unwrap();
        assert_eq!(report.kept_unmanaged, vec!["CLAUDE.md"]);
        assert_eq!(
            std::fs::read(root.path().join("CLAUDE.md")).unwrap(),
            b"existing user edits"
        );
        launch.startup_context.principal = Principal::from(2);
        assert!(ProductAuthority::capture(&launch, root.path()).is_err());
    }

    /// SW-XS-14 M1: a merge-marked `.mcp.json` reaches a managed product root with
    /// managed-record semantics. The fixture's own `.mcp.json` (never written by
    /// CodeSense) is kept untouched, comments and all; once it is gone the merge
    /// is written; a team change reaches the unmodified copy on the next launch;
    /// a user edit is kept; a file outside every repository needs no exclude.
    #[test]
    fn merged_workspace_file_follows_managed_record_semantics() {
        let (home, root, _, authority) = fixture();
        let org_root = crate::context_repo::path_to_org_root(home.path(), &alice(), ORG).unwrap();
        crate::install::merge_marks::set_merge_mark(
            &crate::context_repo::path_to_team_config_dir(&org_root, TEAM),
            ".mcp.json",
            Default::default(),
        )
        .unwrap();
        let team = crate::agents::resolve::tests::content(home.path(), &alice(), TEAM);
        let key = save(
            home.path(),
            &alice(),
            crate::agents::model::Agent {
                startup: vec![".mcp.json".into()],
                ..draft("merger", AgentCli::Claude)
            },
        );
        let launch_now = || {
            launch::prepare(
                home.path(),
                &alice(),
                &key,
                launch::PrepareOptions {
                    target_team: Some((ORG.into(), TEAM.into())),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let dest = root.path().join(".mcp.json");
        let own = "// mine\n{\"mcpServers\": {\"local\": {}}}\n";
        std::fs::write(&dest, own).unwrap();

        // SW-XS-8: through Step 2 itself. A file CodeSense never wrote is kept.
        let report = receive(&authority, &launch_now(), &|_| {}).unwrap();
        assert!(
            report.kept_unmanaged.contains(&".mcp.json".to_string()),
            "{report:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            own,
            "never rewrites it"
        );

        // Free: the merge is written, managed and hidden from git.
        std::fs::remove_file(&dest).unwrap();
        let report = receive(&authority, &launch_now(), &|_| {}).unwrap();
        assert!(
            report.written.contains(&".mcp.json".to_string()),
            "{report:?}"
        );
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&dest).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["slack"]["command"], "slack-mcp");
        let identity = crate::content_engine::context_lifecycle::local_identity(&authority.root);
        let record = super::super::prepare::ManagedRecord::load(home.path(), &alice(), &identity);
        assert_eq!(record.entries[".mcp.json"].source_layer, "generated");

        // Review r1: the merge preview shows exactly what the launch wrote,
        // and the managed-record outcome here.
        let preview = || {
            crate::commands::content_engine::merge_preview_at(
                &authority.ctx,
                &authority.slug,
                &authority.root,
                ".mcp.json",
            )
            .unwrap()
        };
        let shown = preview();
        assert_eq!(shown.text.as_bytes(), std::fs::read(&dest).unwrap());
        assert_eq!(shown.outcome, "unchanged");
        std::fs::write(&dest, "{\"edited\": true}").unwrap();
        assert_eq!(preview().outcome, "keepEdited");
        std::fs::remove_file(&dest).unwrap();
        assert_eq!(preview().outcome, "write");
        let report = receive(&authority, &launch_now(), &|_| {}).unwrap();
        assert!(report.written.contains(&".mcp.json".to_string()));

        // A team change reaches the unmodified copy on relaunch.
        write(
            &team,
            ".mcp.json",
            br#"{"mcpServers": {"slack": {"command": "slack-v2"}}}"#,
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        let report = receive(&authority, &launch_now(), &|_| {}).unwrap();
        assert!(
            report.written.contains(&".mcp.json".to_string()),
            "{report:?}"
        );
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&dest).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["slack"]["command"], "slack-v2");
        let report = receive(&authority, &launch_now(), &|_| {}).unwrap();
        assert!(!report.written.contains(&".mcp.json".to_string()));

        // A user-edited copy (comments included) is kept as it is.
        let edited = "// my edit\n{\"mcpServers\": {}}\n";
        std::fs::write(&dest, edited).unwrap();
        write(
            &team,
            ".mcp.json",
            br#"{"mcpServers": {"slack": {"command": "slack-v3"}}}"#,
        );
        let report = receive(&authority, &launch_now(), &|_| {}).unwrap();
        assert!(
            report.kept_modified.contains(&".mcp.json".to_string()),
            "{report:?}"
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), edited);
    }

    /// SW-XS-8: the interim XS-14 record of a merged file is folded into the
    /// managed record once: the copy it wrote is replaced when unmodified.
    #[test]
    fn the_interim_merged_file_record_is_folded_into_the_managed_record() {
        let (home, root, _, authority) = fixture();
        let dest = root.path().join(".mcp.json");
        std::fs::write(&dest, b"{}").unwrap();
        let canonical = authority.root.clone();
        let key = blake3::hash(canonical.to_string_lossy().as_bytes()).to_hex();
        let interim = home
            .path()
            .join("generated-workspace-files")
            .join(format!("{}.json", &key[..32]));
        std::fs::create_dir_all(interim.parent().unwrap()).unwrap();
        std::fs::write(
            &interim,
            serde_json::to_vec(&serde_json::json!({
                "root": canonical,
                "files": {".mcp.json": blake3::hash(b"{}").to_hex().to_string()},
            }))
            .unwrap(),
        )
        .unwrap();
        super::super::prepare::record::fold_generated_workspace_files(
            home.path(),
            &alice(),
            &authority.root,
        );
        assert!(!interim.exists(), "folded once");
        let identity = crate::content_engine::context_lifecycle::local_identity(&authority.root);
        let record = super::super::prepare::ManagedRecord::load(home.path(), &alice(), &identity);
        assert_eq!(
            record.entries[".mcp.json"].hash.0,
            *blake3::hash(b"{}").as_bytes()
        );
        assert_eq!(record.entries[".mcp.json"].source_layer, "generated");
        // The clone's shared block keeps the entry until a Step 2 has
        // rewritten this workspace's own block (review r1).
        assert_eq!(
            record.shared_exclude_pending,
            vec!["/.mcp.json".to_string()]
        );
    }

    /// Review r1: once Step 2 rewrote the workspace's own block, the shared
    /// block's interim entry goes (and only then).
    #[test]
    fn shared_interim_excludes_are_released_after_step_two() {
        let (_home, _root, _, authority) = fixture();
        let root = &authority.root;
        if !root.join(".git").exists() {
            assert!(std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(root)
                .status()
                .unwrap()
                .success());
        }
        crate::content_engine::git_exclude::add_entries(root, &["/.mcp.json"]).unwrap();
        let mut record = super::super::prepare::ManagedRecord {
            shared_exclude_pending: vec!["/.mcp.json".into()],
            ..Default::default()
        };
        assert!(super::super::prepare::record::release_shared_excludes(
            &mut record,
            root
        ));
        assert!(record.shared_exclude_pending.is_empty());
        assert!(!crate::content_engine::read_managed_entries(root)
            .unwrap_or_default()
            .iter()
            .any(|entry| entry == "/.mcp.json"));
    }

    /// Review r2: tearing a workspace down gives back the shared-block entries
    /// an unfolded interim record left for it.
    #[test]
    fn teardown_releases_interim_shared_excludes() {
        let (home, _root, _, authority) = fixture();
        let root = &authority.root;
        if !root.join(".git").exists() {
            assert!(std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(root)
                .status()
                .unwrap()
                .success());
        }
        crate::content_engine::git_exclude::add_entries(root, &["/.mcp.json"]).unwrap();
        let key = blake3::hash(root.to_string_lossy().as_bytes()).to_hex();
        let interim = home
            .path()
            .join("generated-workspace-files")
            .join(format!("{}.json", &key[..32]));
        std::fs::create_dir_all(interim.parent().unwrap()).unwrap();
        std::fs::write(
            &interim,
            serde_json::to_vec(&serde_json::json!({
                "root": root,
                "files": {".mcp.json": blake3::hash(b"{}").to_hex().to_string()},
            }))
            .unwrap(),
        )
        .unwrap();
        super::super::prepare::record::release_on_teardown(home.path(), &alice(), root);
        assert!(!interim.exists());
        assert!(!crate::content_engine::read_managed_entries(root)
            .unwrap_or_default()
            .iter()
            .any(|entry| entry == "/.mcp.json"));
    }

    /// Review r1: the boot sweep deletes an interim record whose workspace is
    /// gone and leaves one it can't attribute to exactly one user.
    #[test]
    fn the_boot_sweep_drops_records_of_gone_workspaces() {
        let (home, _root, _, authority) = fixture();
        let dir = home.path().join("generated-workspace-files");
        std::fs::create_dir_all(&dir).unwrap();
        let gone = dir.join("gone.json");
        std::fs::write(
            &gone,
            serde_json::to_vec(&serde_json::json!({
                "root": home.path().join("no-such-workspace"),
                "files": {".mcp.json": blake3::hash(b"{}").to_hex().to_string()},
            }))
            .unwrap(),
        )
        .unwrap();
        let unowned = dir.join("unowned.json");
        std::fs::write(
            &unowned,
            serde_json::to_vec(&serde_json::json!({
                "root": authority.root,
                "files": {".mcp.json": blake3::hash(b"{}").to_hex().to_string()},
            }))
            .unwrap(),
        )
        .unwrap();
        super::super::prepare::record::sweep_interim_records(home.path());
        assert!(!gone.exists());
        assert!(unowned.exists(), "left for that workspace's next launch");
    }

    #[test]
    fn direct_context_and_arbitrary_roots_are_not_applicable() {
        let (_home, _root, launch, authority) = fixture();
        let arbitrary = tempfile::tempdir().unwrap();
        assert!(ProductAuthority::capture(&launch, arbitrary.path())
            .unwrap()
            .is_none());
        let source =
            crate::agents::resolve::tests::content(&authority.ctx.codesense_home, &alice(), TEAM);
        assert!(ProductAuthority::capture(&launch, &source)
            .unwrap()
            .is_none());
    }

    #[test]
    fn ownership_and_source_overlap_are_revalidated() {
        let (_home, root, launch, authority) = fixture();
        let store =
            SyncStateStore::open(default_store_path(&authority.ctx.codesense_home)).unwrap();
        let second = crate::domain::derive_epic_workspace_slug("another-product").unwrap();
        crate::content_engine::register_attached_code_repo(&authority.ctx, &second, root.path())
            .unwrap();
        assert!(authority.validate().is_err());
        assert_eq!(store.product_registrations(root.path()).unwrap().len(), 2);
        let source =
            crate::agents::resolve::tests::content(&authority.ctx.codesense_home, &alice(), TEAM);
        crate::content_engine::register_attached_code_repo(
            &authority.ctx,
            &authority.slug,
            &source,
        )
        .unwrap();
        assert!(ProductAuthority::capture(&launch, &source)
            .unwrap_err()
            .contains("overlaps"));
    }
}
