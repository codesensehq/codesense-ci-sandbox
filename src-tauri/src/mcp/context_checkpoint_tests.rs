// SPDX-License-Identifier: MIT
use super::*;
use crate::agents::prepare::step3::tests::{git, write, Fx};
use crate::agents::resolve::tests::{alice, ORG, TEAM};
use crate::mcp::context_operations::{bind_context, ContextSelection};

#[test]
fn every_context_tool_accepts_the_same_context_argument_in_each_input_form() {
    use serde_json::json;
    for context in [
        json!({"kind":"team"}),
        json!({"kind":"worktree","id":"cw_sprint"}),
        json!({"kind":"launcher"}),
    ] {
        let target = json!({"kind":"local","codeRepoPath":"/workspace","sessionId":"session"});
        let hydrate: HydrateWorktreeParams =
            serde_json::from_value(json!({"target":target,"context":context})).unwrap();
        let SelectedHydrateRequest::Explicit(hydrate) = hydrate.into_request().unwrap() else {
            panic!()
        };
        assert!(hydrate.context.is_some());
        let publish: ReverseSyncWorktreeToolParams =
            serde_json::from_value(json!({"target":target,"context":context})).unwrap();
        let SelectedPublishRequest::Explicit(publish) = publish.into_request().unwrap() else {
            panic!()
        };
        assert!(publish.context.is_some());
        let preview: PreviewReverseSyncParams =
            serde_json::from_value(json!({"target":target,"context":context})).unwrap();
        let SelectedPreviewRequest::Explicit(preview) = preview.into_request().unwrap() else {
            panic!()
        };
        assert!(preview.context.is_some());
        let story: crate::mcp::context_operations::explicit::HydrateStoryParams =
            serde_json::from_value(json!({"target":target,"storyId":"TASK","context":context}))
                .unwrap();
        assert!(story.context.is_some());
        let count: SyncStatusParams = serde_json::from_value(json!({"context":context})).unwrap();
        assert!(count.context.is_some());
        let legacy: HydrateWorktreeParams = serde_json::from_value(
            json!({"codeRepoPath":"/workspace","sessionId":"session","context":context}),
        )
        .unwrap();
        let SelectedHydrateRequest::Legacy(legacy) = legacy.into_request().unwrap() else {
            panic!()
        };
        assert!(legacy.context.is_some());
    }
}

#[test]
fn context_selection_defaults_to_clone_and_rejects_stale_or_other_org_ids() {
    let fx = Fx::new();
    let row = fx.context("Sprint");
    let mut ctx = crate::content_engine::ResolverContext {
        context_root: None,
        codesense_home: fx.home.path().into(),
        user_id: 1,
        org_slug: ORG.into(),
        team_name: Some(TEAM.into()),
    };
    bind_context(
        &mut ctx,
        Some(ContextSelection::Worktree { id: row.id.clone() }),
        None,
    )
    .unwrap();
    assert_eq!(ctx.context_root.as_ref().unwrap().root, row.root);
    bind_context(&mut ctx, None, Some(&row.id)).unwrap();
    assert!(
        ctx.context_root.is_none(),
        "a launcher's context is never implicit"
    );
    bind_context(&mut ctx, Some(ContextSelection::Launcher), Some(&row.id)).unwrap();
    assert!(
        bind_context(&mut ctx, Some(ContextSelection::Launcher), None)
            .unwrap_err()
            .contains("launcherContext")
    );
    assert!(bind_context(
        &mut ctx,
        Some(ContextSelection::Worktree {
            id: "cw_missing".into()
        }),
        None
    )
    .is_err());
    ctx.org_slug = "other/context".into();
    assert!(bind_context(
        &mut ctx,
        Some(ContextSelection::Worktree { id: row.id.clone() }),
        None
    )
    .is_err());
    ctx.org_slug = ORG.into();
    std::fs::remove_dir_all(&row.root).unwrap();
    assert!(bind_context(
        &mut ctx,
        Some(ContextSelection::Worktree { id: row.id }),
        None
    )
    .unwrap_err()
    .contains("no longer exists"));
}

fn resolve_nav_patch(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    teams: &[String],
    active: &str,
    patch: serde_json::Value,
) -> Result<(serde_json::Value, serde_json::Value, bool), String> {
    let (patch, described) = super::super::nav_lens::validate_patch_at(
        home,
        principal,
        &patch,
        &serde_json::json!({}),
        teams,
        true,
    )?;
    let shown = patch["navLens"]
        .as_object()
        .is_some_and(|l| l.contains_key(active));
    Ok((patch, described, shown))
}

#[test]
fn nav_patch_resolves_name_id_clone_and_rejects_the_whole_patch_on_a_bad_target() {
    use serde_json::json;
    let fx = Fx::new();
    let row = fx.context("Sprint");
    let key = format!("{ORG}:{TEAM}");
    let teams = vec![key.clone(), format!("{ORG}:design")];
    for source in ["sPrInT", row.id.as_str()] {
        let (patch, described, shown) = resolve_nav_patch(
            fx.home.path(),
            &alice(),
            &teams,
            &key,
            json!({"navLens":{key.clone():{"source":source,"lens":"blocked"}}}),
        )
        .unwrap();
        assert_eq!(
            patch["navLens"][&key],
            json!({"source":row.id,"view":"changes"})
        );
        assert_eq!(patch["navCollapsed"][&key], false);
        assert_eq!(described[&key]["source"]["name"], "Sprint");
        assert!(shown);
    }
    let (_, described, shown) = resolve_nav_patch(
        fx.home.path(),
        &alice(),
        &teams,
        "other:team",
        json!({"navLens":{key.clone():{"source":"clone"}}}),
    )
    .unwrap();
    assert_eq!(described[&key]["source"], "clone");
    assert!(!shown);
    for lens in [
        json!({key.clone():{"source":"missing"}}),
        json!({format!("{ORG}:design"): {"source":row.id}}),
        json!({"bad:team":{"source":"clone"}}),
        json!({key.clone():{"source":row.id},"bad:team":{"source":"clone"}}),
    ] {
        let patch = json!({"rightPaneOpen":false,"navLens":lens});
        let state = McpViewerState::default();
        assert!(resolve_nav_patch(fx.home.path(), &alice(), &teams, &key, patch).is_err());
        assert!(state.get().get("rightPaneOpen").is_none());
    }
}

#[test]
fn worktree_changes_use_merge_base_show_committed_and_uncommitted_files_and_keep_main_advances_out()
{
    let fx = Fx::new();
    write(&fx.team, "docs/base.md", b"base\n");
    let row = fx.context("Sprint");
    let canonical = crate::context_repo::path_to_org_root(fx.home.path(), &alice(), ORG).unwrap();
    write(&row.launch_cwd, "docs/committed.md", b"committed\n");
    git(&row.root, &["add", "."]);
    git(&row.root, &["commit", "-qm", "branch edit"]);
    write(&row.launch_cwd, "docs/draft.md", b"draft\n");
    write(&fx.team, "docs/main-only.md", b"main advanced\n");
    git(&canonical, &["add", "."]);
    git(&canonical, &["commit", "-qm", "main edit"]);
    let before = git(&canonical, &["show-ref"]);
    let set = commands::git_sync::context_worktree::publish_set(&row.root, None).unwrap();
    let paths: Vec<_> = set.general.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&format!("{TEAM}/content/docs/committed.md").as_str()));
    assert!(paths.contains(&format!("{TEAM}/content/docs/draft.md").as_str()));
    assert!(!paths.contains(&format!("{TEAM}/content/docs/main-only.md").as_str()));
    let status = commands::git_sync::context_worktree::status_impl(&row, vec![]).unwrap();
    assert_eq!(status["ahead"], 1);
    assert_eq!(status["behind"], 1);
    assert_eq!(status["uncommitted"], 1);
    let committed = commands::git_sync::context_worktree::redline(
        &row,
        &format!("{TEAM}/content/docs/committed.md"),
        None,
    )
    .unwrap();
    assert!(committed.before_label.unwrap().starts_with("main · "));
    assert!(!committed.after_label.unwrap().contains("uncommitted"));
    let draft = commands::git_sync::context_worktree::redline(
        &row,
        &format!("{TEAM}/content/docs/draft.md"),
        None,
    )
    .unwrap();
    assert_eq!(draft.after_label.as_deref(), Some("Sprint · uncommitted"));
    assert_eq!(git(&canonical, &["show-ref"]), before);
}

#[test]
fn context_worktree_status_keeps_merge_conflicts_and_unfinished_git_operations() {
    let fx = Fx::new();
    write(&fx.team, "docs/conflict.md", b"base\n");
    let row = fx.context("Merge");
    let canonical = crate::context_repo::path_to_org_root(fx.home.path(), &alice(), ORG).unwrap();
    write(&row.launch_cwd, "docs/conflict.md", b"branch edit\n");
    git(&row.root, &["add", "."]);
    git(&row.root, &["commit", "-qm", "branch edit"]);
    write(&fx.team, "docs/conflict.md", b"main edit\n");
    git(&canonical, &["add", "."]);
    git(&canonical, &["commit", "-qm", "main edit"]);
    let merge = std::process::Command::new("git")
        .arg("-C")
        .arg(&row.root)
        .args(["merge", "--no-edit", "main"])
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert_eq!(merge.status.code(), Some(1));
    let before = git(&canonical, &["show-ref"]);
    let status = commands::git_sync::context_worktree::status_impl(&row, vec![]).unwrap();
    assert_eq!(status["ahead"], 1);
    assert_eq!(status["behind"], 1);
    assert_eq!(status["upstream"], "main");
    assert_eq!(status["conflicts"].as_array().unwrap().len(), 1);
    assert_eq!(
        status["conflicts"][0]["path"],
        format!("{TEAM}/content/docs/conflict.md")
    );
    assert_eq!(status["sync"]["noSender"], true);
    assert_eq!(status["sync"]["lastSyncedAt"], serde_json::Value::Null);

    // Staging a resolution still leaves the merge open until an agent commits it.
    write(&row.launch_cwd, "docs/conflict.md", b"resolved\n");
    git(&row.root, &["add", "."]);
    let status = commands::git_sync::context_worktree::status_impl(&row, vec![]).unwrap();
    assert!(status["conflicts"].as_array().unwrap().is_empty());
    assert_eq!(status["sync"]["state"], "syncing");
    assert_eq!(status["sync"]["progress"], "Merging");
    assert_eq!(git(&canonical, &["show-ref"]), before);
}

#[test]
fn context_status_refuses_ambiguous_names_and_still_accepts_ids() {
    use crate::install::worktree_index::{resolve_context_record, write_context_record};
    let fx = Fx::new();
    let mut sunny = fx.context("sunny-sprint");
    sunny.name = "Équipe".into();
    write_context_record(fx.home.path(), &alice(), &sunny).unwrap();
    let mut design = fx.context("design-sprint");
    design.name = sunny.name.clone();
    design.team = "design".into();
    design.launch_cwd = design.root.join("design/content");
    std::fs::create_dir_all(&design.launch_cwd).unwrap();
    design.last_used_at = 99;
    write_context_record(fx.home.path(), &alice(), &design).unwrap();
    let error = resolve_context_record(fx.home.path(), &alice(), ORG, None, "équipe").unwrap_err();
    assert!(error.contains("Ambiguous"));
    assert!(error.contains(&sunny.id) && error.contains(&design.id));
    assert_eq!(
        resolve_context_record(fx.home.path(), &alice(), ORG, None, &sunny.id)
            .unwrap()
            .id,
        sunny.id
    );
    assert_eq!(
        resolve_context_record(fx.home.path(), &alice(), ORG, Some(TEAM), "équipe")
            .unwrap()
            .id,
        sunny.id
    );
}

#[test]
fn a_local_connection_keeps_its_launcher_context_for_a_remote_target() {
    use crate::mcp::context_operations::WorkspaceTarget;
    let fx = Fx::new();
    let row = fx.context("launcher-remote");
    let state = crate::commands::dashboard_launch::DashboardLaunchState::new(4);
    let record = crate::commands::dashboard_launch::LaunchRecord {
        meta: crate::commands::dashboard_launch::LaunchMeta {
            launcher_context: Some(row.id.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    state.begin_with_provenance("child", None, record).unwrap();
    state.bind("child", "child-session", None).unwrap();
    let target = WorkspaceTarget::Remote {
        machine_id: "machine".into(),
        workspace_handle: "workspace".into(),
    };
    let session = target
        .context_session(Some("child-session".into()))
        .unwrap();
    let mut ctx = crate::content_engine::ResolverContext {
        context_root: None,
        codesense_home: fx.home.path().into(),
        user_id: 1,
        org_slug: ORG.into(),
        team_name: Some(TEAM.into()),
    };
    bind_context(
        &mut ctx,
        Some(ContextSelection::Launcher),
        state.launcher_context(&session).as_deref(),
    )
    .unwrap();
    assert_eq!(ctx.context_root.unwrap().id, row.id);
}
