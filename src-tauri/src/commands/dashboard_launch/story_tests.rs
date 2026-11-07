// SPDX-License-Identifier: MIT
#[test]
fn cde36_record_uses_the_real_story_and_never_the_workspace_handle() {
    let (state, mut snapshot) = bound_launch(SessionOwner {
        principal: None,
        org: "acme".into(),
        team: Some("dev".into()),
    });
    for story in [Some("SV-2"), None] {
        snapshot.meta.story = story.map(str::to_string);
        let record = record_from_launch(
            "sess-1",
            &snapshot,
            state.owner_for_session("sess-1").as_ref(),
            &launch_facts(&[]),
        );
        assert_eq!(record.story_id.as_deref(), story);
        if story.is_none() {
            assert_eq!(
                record.unavailable["storyId"],
                "the launch belongs to no story"
            );
        }
    }
}

#[test]
fn cde36_inheritance_comes_from_launcher_meta_and_explicit_story_wins() {
    for explicit in [None, Some("SV-2")] {
        let state = DashboardLaunchState::new(4);
        state
            .begin_with_provenance(
                "lead",
                None,
                LaunchRecord {
                    meta: LaunchMeta {
                        story: Some("SV-1".into()),
                        team: Some(LaunchTeam {
                            org: "acme".into(),
                            team: "dev".into(),
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        state.bind("lead", "lead-tab", None).unwrap();
        state
            .begin_with_provenance(
                "child",
                None,
                LaunchRecord {
                    meta: LaunchMeta {
                        launched_by: Some("lead-tab".into()),
                        team: Some(LaunchTeam {
                            org: "acme".into(),
                            team: "dev".into(),
                        }),
                        story: explicit.map(str::to_string),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        state.bind("child", "child-tab", None).unwrap();
        let write =
            compose_launch_write(&state, "child-tab", "child", task_facts(Some("Build it")))
                .unwrap();
        assert_eq!(write.record.story_id.as_deref(), explicit.or(Some("SV-1")));
        assert_eq!(
            state
                .snapshot()
                .into_iter()
                .find(|s| s.handle == "child")
                .unwrap()
                .meta
                .story
                .as_deref(),
            explicit.or(Some("SV-1"))
        );
        assert_eq!(write.launcher.as_deref(), Some("lead-tab"));
    }
}

#[cfg(test)]
fn cde36_usage(session: &str, cwd: &Path) -> crate::session_usage::event::UsageEvent {
    crate::session_usage::event::UsageEvent {
        cli: codesense_pty_core::agent_cli::AgentCli::Claude,
        event_id: format!("request-{session}"),
        session_id: session.into(),
        at_ms: chrono::Utc::now().timestamp_millis(),
        cwd: Some(cwd.to_string_lossy().into_owned()),
        model: "claude-opus-4-8".into(),
        tokens: crate::session_usage::event::Tokens {
            input: 10,
            ..Default::default()
        },
        reported_cost_usd: Some(0.25),
    }
}

fn cde36_story_worker(path: &Path, cwd: &Path) -> DashboardLaunchState {
    let state = DashboardLaunchState::new(4);
    state.set_task_store_path_for_test(path.to_path_buf());
    bind_launch(&state, "context-worker", "worker", None, "SV-1", None);
    state.set_worktree_dir_if_absent("context-worker", cwd.to_path_buf());
    state
}

#[tokio::test]
async fn cde36_launch_link_survives_both_usage_orders_a_duplicate_and_store_reopen() {
    use crate::session_usage::{Filter, StoryLink, StoryResolver, UsageStore};
    for usage_first in [false, true] {
        let (dir, cache) = org_cache_with_story("dev", "SV-1");
        let path = dir.path().join("usage.sqlite");
        let cwd = dir.path().join("content-worktree");
        std::fs::create_dir_all(&cwd).unwrap();
        let state = cde36_story_worker(&path, &cwd);
        let event = cde36_usage("worker", &cwd);
        if usage_first {
            let mut store = UsageStore::open(&path).unwrap();
            store.ingest(&[event.clone()]).unwrap();
            store
                .link_stories(
                    &[("worker".into(), cwd.to_string_lossy().into_owned())],
                    || StoryResolver::for_home(path.parent().unwrap(), Default::default()),
                )
                .unwrap();
        }
        let story = resolve_story_path_in_for(
            &cache,
            "SV-1",
            &crate::context_repo::caller_scope::CallerScope::Human,
        )
        .unwrap();
        for _ in 0..2 {
            let source = PathBuf::from(&story.story_file_path);
            write_launch_record_resolving(
                &state,
                "worker",
                "context-worker",
                task_facts(Some("Build")),
                move |record, _| async move {
                    Some(StoryLink {
                        story_id: record.story_id.unwrap(),
                        source,
                        org: "acme".into(),
                    })
                },
            );
            let mut captured = state
                .task_captures
                .lock()
                .unwrap()
                .get("worker")
                .unwrap()
                .0
                .clone();
            let _ = captured.changed().await;
        }
        if !usage_first {
            UsageStore::open(&path).unwrap().ingest(&[event]).unwrap();
        }
        state.release_for_session("worker");
        let store = UsageStore::open(&path).unwrap();
        let report = store
            .report_with(
                &Filter {
                    story: Some("SV-1".into()),
                    ..Default::default()
                },
                &mut StoryResolver::default(),
            )
            .unwrap();
        assert_eq!(report.sessions.len(), 1, "usage first: {usage_first}");
        assert_eq!(report.sessions[0].session_id, "worker");
        assert_eq!(report.sessions[0].cost_usd, Some(0.25));
        assert_eq!(
            store
                .read_session_record("worker")
                .unwrap()
                .unwrap()
                .story_id
                .as_deref(),
            Some("SV-1")
        );
        assert_eq!(
            store
                .read_task("worker")
                .unwrap()
                .unwrap()
                .story_id
                .as_deref(),
            Some("SV-1")
        );
    }
}

thread_local! {
    pub(super) static CDE36_RESERVATION_GAP: std::cell::RefCell<Option<Box<dyn FnOnce(&DashboardLaunchState)>>> = const { std::cell::RefCell::new(None) };
}

#[test]
fn cde36_inherited_story_survives_a_launcher_change_during_the_store_lookup() {
    let state = DashboardLaunchState::new(4);
    state
        .begin_with_provenance(
            "lead",
            None,
            LaunchRecord {
                meta: LaunchMeta {
                    story: Some("SV-1".into()),
                    team: Some(LaunchTeam {
                        org: "acme".into(),
                        team: "dev".into(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    state.bind("lead", "lead-tab", None).unwrap();
    CDE36_RESERVATION_GAP.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|state| {
            state.lock().by_handle.get_mut("lead").unwrap().meta.story = Some("SV-2".into());
        }))
    });
    state
        .begin_with_provenance(
            "child",
            None,
            LaunchRecord {
                meta: LaunchMeta {
                    launched_by: Some("lead-tab".into()),
                    team: Some(LaunchTeam {
                        org: "acme".into(),
                        team: "dev".into(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    let rows = state.snapshot();
    assert_eq!(
        rows.iter()
            .find(|s| s.handle == "child")
            .unwrap()
            .meta
            .story
            .as_deref(),
        Some("SV-1")
    );
    assert_eq!(
        rows.iter()
            .find(|s| s.handle == "lead")
            .unwrap()
            .meta
            .story
            .as_deref(),
        Some("SV-2")
    );
    CDE36_RESERVATION_GAP.with(|hook| hook.borrow_mut().take());
}

#[test]
fn cde36_a_routine_dispatch_keeps_its_run_but_has_no_story() {
    let state = DashboardLaunchState::new(4);
    let handle = "routine:acme/dev/digest@run-1";
    state
        .begin_with_provenance(
            handle,
            Some(RunProvenance {
                run_id: "run-1".into(),
                story_id: handle.into(),
                routine_key: Some("acme/dev/digest".into()),
                routine: Some(RoutineBrief {
                    name: "Digest".into(),
                    task: "Summarise".into(),
                }),
                ..Default::default()
            }),
            LaunchRecord::default(),
        )
        .unwrap();
    state.bind(handle, "routine-tab", None).unwrap();
    let write = compose_launch_write(&state, "routine-tab", handle, task_facts(None)).unwrap();
    assert_eq!(write.record.story_id, None);
    assert_eq!(write.record.run_id.as_deref(), Some("run-1"));
    assert_eq!(
        write.record.unavailable["storyId"],
        "the launch belongs to no story"
    );
}

#[tokio::test]
async fn cde36_the_worktrees_story_wins_over_the_launch_in_both_usage_orders() {
    use crate::session_usage::{Filter, StoryLink, StoryResolver, UsageStore};
    for usage_first in [false, true] {
        let (dir, cache) = org_cache_with_story("dev", "SV-1");
        let home = dir.path();
        let path = home.join("usage.sqlite");
        let cwd = home.join("story-worktree");
        std::fs::create_dir_all(&cwd).unwrap();
        let source = home.join("SV-2.md");
        std::fs::write(
            &source,
            "---\ntype: story\nstory_id: SV-2\ntitle: Actual worktree\n---\n",
        )
        .unwrap();
        let worktree = crate::install::worktree_index::WorktreeStoryRecord::new(
            cwd.clone(),
            "acme",
            crate::domain::SourceRepoSlug::new("acme", "app").unwrap(),
            "SV-2",
            source.clone(),
        );
        crate::install::worktree_index::write_record(
            home,
            &crate::domain::Principal::from(42u64),
            &worktree,
        )
        .unwrap();
        let state = cde36_story_worker(&path, &cwd);
        let event = cde36_usage("worker", &cwd);
        if usage_first {
            let mut store = UsageStore::open(&path).unwrap();
            store.ingest(&[event.clone()]).unwrap();
            store
                .link_stories(
                    &[("worker".into(), cwd.to_string_lossy().into_owned())],
                    || StoryResolver::for_home(path.parent().unwrap(), Default::default()),
                )
                .unwrap();
        }
        let resolved = resolve_story_path_in_for(
            &cache,
            "SV-1",
            &crate::context_repo::caller_scope::CallerScope::Human,
        )
        .unwrap();
        write_launch_record_resolving(
            &state,
            "worker",
            "context-worker",
            task_facts(Some("Build")),
            move |record, _| async move {
                Some(StoryLink {
                    story_id: record.story_id.unwrap(),
                    source: resolved.story_file_path.into(),
                    org: "acme".into(),
                })
            },
        );
        let mut captured = state
            .task_captures
            .lock()
            .unwrap()
            .get("worker")
            .unwrap()
            .0
            .clone();
        let _ = captured.changed().await;
        if !usage_first {
            UsageStore::open(&path).unwrap().ingest(&[event]).unwrap();
        }
        // No live registry or worktree remains: the launch writer must have kept the cwd link.
        crate::install::worktree_index::remove_record(
            home,
            &crate::domain::Principal::from(42u64),
            &cwd,
        )
        .unwrap();
        state.release_for_session("worker");
        let store = UsageStore::open(&path).unwrap();
        store
            .link_story(
                "worker",
                &StoryLink {
                    story_id: "SV-1".into(),
                    source,
                    org: "acme".into(),
                },
            )
            .unwrap();
        let report = store
            .report_with(
                &Filter {
                    story: Some("SV-2".into()),
                    ..Default::default()
                },
                &mut StoryResolver::default(),
            )
            .unwrap();
        assert_eq!(report.sessions.len(), 1, "usage first: {usage_first}");
        assert_eq!(
            report.sessions[0].story.as_ref().unwrap().title.as_deref(),
            Some("Actual worktree")
        );
    }
}

#[test]
fn cde36_launch_source_uses_its_own_org_when_the_selected_org_changes_or_is_absent() {
    use crate::context_repo::caller_scope::CallerScope;
    use crate::install::frontmatter_cache::FrontmatterCache;
    use crate::mcp::state::{ActiveTeam, ActiveTeamState};
    let home = tempfile::tempdir().unwrap();
    let principal = Principal::from(42u64);
    let root =
        |org: &str| crate::context_repo::path_to_org_root(home.path(), &principal, org).unwrap();
    for org in ["acme/context", "other/context"] {
        let content = root(org).join("dev/content");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::write(
            content.join("story.md"),
            "---\ntype: story\nstory_id: SV-1\nsource_repo: acme/app\n---\n# Story\n",
        )
        .unwrap();
    }
    for selected in [Some("other/context"), Some("acme/context"), None] {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let cache = selected
            .map(|org| FrontmatterCache::build_from_scan(&root(org)))
            .unwrap_or_else(FrontmatterCache::new);
        app.manage(FrontmatterCache::shared(cache));
        app.manage(std::sync::Arc::new(
            crate::install::org_story_resolver::OrgStoryResolver::new(),
        ));
        let selected_state = ActiveTeamState::default();
        if let Some(org) = selected {
            selected_state.update(ActiveTeam {
                org_slug: org.into(),
                team_name: "dev".into(),
            });
        }
        app.manage(selected_state);
        let active = launch_story_context(app.handle(), home.path().into(), 42);
        let scope = CallerScope::team(
            home.path(),
            &principal,
            &LaunchTeam {
                org: "acme/context".into(),
                team: "dev".into(),
            },
        );
        let resolved =
            resolve_story_path_in_org_for(app.handle(), &active, "acme/context", "SV-1", &scope)
                .unwrap();
        assert_eq!(
            PathBuf::from(resolved.story_file_path),
            root("acme/context").join("dev/content/story.md")
        );
    }
}

#[tokio::test]
async fn cde36_app_writer_keeps_same_team_story_but_never_inherits_across_teams_or_orgs() {
    use crate::commands::local_profile::{LocalProfileState, MachineOwner};
    use crate::install::frontmatter_cache::FrontmatterCache;
    use crate::mcp::state::{ActiveTeam, ActiveTeamState};
    use crate::session_usage::{Filter, StoryResolver, UsageStore};
    let env = crate::routines::test_support::TempHome::new();
    let home = env.codesense_home();
    let principal = Principal::from(42u64);
    let root = |org: &str| crate::context_repo::path_to_org_root(&home, &principal, org).unwrap();
    for org in ["acme/context", "other/context"] {
        let content = root(org).join("dev/content");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::write(
            content.join("story.md"),
            "---\ntype: story\nstory_id: SV-1\nsource_repo: acme/app\n---\n# Story\n",
        )
        .unwrap();
    }
    for (i, (org, team, explicit, expected, parent_team)) in [
        ("acme/context", "dev", None, Some("SV-1"), "dev"),
        ("other/context", "dev", None, None, "dev"),
        ("acme/context", "other", None, None, "dev"),
        ("other/context", "dev", Some("SV-1"), Some("SV-1"), "dev"),
        ("acme/context", "other", Some("SV-1"), Some("SV-1"), "dev"),
        ("acme/context", "other", None, Some("SV-1"), "other"),
    ]
    .into_iter()
    .enumerate()
    {
        if team == "other" {
            // The ID was known when launched; its source disappears before capture.
            std::fs::write(
                root(org).join("dev/content/story.md"),
                "---\ntype: story\nstory_id: SV-1\nsource_repo: acme/app\n---\n# Story\n",
            )
            .unwrap();
        }
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(LocalProfileState::for_test(
            &home,
            42,
            MachineOwner::LegacyGithub { id: 42 },
        ));
        // A different org's cache is resident while the target comes from launch metadata.
        app.manage(FrontmatterCache::shared(FrontmatterCache::build_from_scan(
            &root("other/context"),
        )));
        app.manage(std::sync::Arc::new(
            crate::install::org_story_resolver::OrgStoryResolver::new(),
        ));
        let selected = ActiveTeamState::default();
        selected.update(ActiveTeam {
            org_slug: "other/context".into(),
            team_name: "dev".into(),
        });
        app.manage(selected);
        let path = home.join("usage.sqlite");
        let state = DashboardLaunchState::new(4);
        state.set_task_store_path_for_test(path.clone());
        state
            .begin_with_provenance(
                "lead",
                None,
                LaunchRecord {
                    meta: LaunchMeta {
                        story: Some("SV-1".into()),
                        team: Some(LaunchTeam {
                            org: "acme/context".into(),
                            team: parent_team.into(),
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        state.bind("lead", "lead-tab", None).unwrap();
        state
            .begin_with_provenance(
                "child",
                Some(RunProvenance {
                    story_id: "workspace-handle".into(),
                    ..Default::default()
                }),
                LaunchRecord {
                    meta: LaunchMeta {
                        story: explicit.map(str::to_string),
                        launched_by: Some("lead-tab".into()),
                        team: Some(LaunchTeam {
                            org: org.into(),
                            team: team.into(),
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        let session = format!("app-worker-{i}");
        state.bind("child", &session, None).unwrap();
        let cwd = home.join(format!("content-worktree-{i}"));
        std::fs::create_dir_all(&cwd).unwrap();
        state.set_worktree_dir_if_absent("child", cwd.clone());
        if team == "other" {
            std::fs::remove_file(root(org).join("dev/content/story.md")).unwrap();
        }
        write_launch_record_for_app(
            app.handle(),
            &state,
            &session,
            "child",
            task_facts(Some("Build")),
        );
        let mut capture = state
            .task_captures
            .lock()
            .unwrap()
            .get(&session)
            .unwrap()
            .0
            .clone();
        let _ = capture.changed().await;
        UsageStore::open(&path)
            .unwrap()
            .ingest(&[cde36_usage(&session, &cwd)])
            .unwrap();
        let live = UsageStore::open(&path)
            .unwrap()
            .report_with(
                &Filter {
                    story: Some("SV-1".into()),
                    ..Default::default()
                },
                &mut StoryResolver::default(),
            )
            .unwrap();
        assert_eq!(state.resolve("child").as_deref(), Some(session.as_str()));
        assert_eq!(
            live.sessions.iter().any(|s| s.session_id == session),
            expected.is_some()
        );
        state.release_for_session(&session);
        let store = UsageStore::open(&path).unwrap();
        let record = store.read_session_record(&session).unwrap().unwrap();
        assert_eq!(record.story_id.as_deref(), expected);
        assert_eq!(record.org.as_deref(), Some(org));
        let expected_usage = expected;
        let has_link = expected.is_some() && (team != "other" || explicit.is_some());
        if has_link {
            let conn = rusqlite::Connection::open(&path).unwrap();
            let (source, linked_org): (Option<String>, String) = conn
                .query_row(
                    "SELECT story_path, org FROM session_story WHERE session_id = ?1",
                    [&session],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(linked_org, org);
            if team == "other" {
                assert_eq!(
                    source, None,
                    "an explicit but unreadable story keeps only its known id"
                );
            } else {
                assert_eq!(
                    source.as_deref(),
                    Some(root(org).join("dev/content/story.md").to_str().unwrap())
                );
            }
        }
        if !has_link {
            let count: i64 = rusqlite::Connection::open(&path)
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM session_story WHERE session_id = ?1",
                    [&session],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "record-only fallback must not backfill a link");
        }
        let report = store
            .report_with(&Filter::default(), &mut StoryResolver::default())
            .unwrap();
        let usage = report
            .sessions
            .iter()
            .find(|s| s.session_id == session)
            .unwrap();
        assert_eq!(usage.story.as_ref().map(|s| s.id.as_str()), expected_usage);
        if team == "other" && explicit.is_some() {
            let story = usage.story.as_ref().unwrap();
            assert_eq!(
                (&story.title, &story.status, &story.epic),
                (&None, &None, &None)
            );
        }
        let filtered = UsageStore::open(&path)
            .unwrap()
            .report_with(
                &Filter {
                    story: Some("SV-1".into()),
                    ..Default::default()
                },
                &mut StoryResolver::default(),
            )
            .unwrap();
        assert_eq!(
            filtered.sessions.iter().any(|s| s.session_id == session),
            expected_usage.is_some()
        );
        assert_eq!(
            store
                .read_task(&session)
                .unwrap()
                .unwrap()
                .story_id
                .as_deref(),
            expected
        );
    }
}
fn cde36_native_export(native: &str) -> serde_json::Value {
    serde_json::json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [{"attributes": [
        {"key": "event.name", "value": {"stringValue": "api_request"}},
        {"key": "request_id", "value": {"stringValue": format!("request-{native}")}},
        {"key": "session.id", "value": {"stringValue": native}},
        {"key": "event.timestamp", "value": {"stringValue": "2026-10-16T18:52:24.455Z"}},
        {"key": "model", "value": {"stringValue": "claude-opus-4-8"}},
        {"key": "input_tokens", "value": {"intValue": 10}}
    ]}]}]}]})
}

#[tokio::test]
async fn cde36_r1_writer_link_follows_native_telemetry_after_reopen() {
    use crate::session_usage::{
        ingest_telemetry_for_at, Filter, StoryLink, StoryResolver, UsageStore,
    };
    for usage_first in [false, true] {
        let (dir, cache) = org_cache_with_story("dev", "SV-1");
        let path = dir.path().join("usage.sqlite");
        let cwd = dir.path().join("content-worktree");
        std::fs::create_dir_all(&cwd).unwrap();
        let state = cde36_story_worker(&path, &cwd);
        let story = resolve_story_path_in_for(
            &cache,
            "SV-1",
            &crate::context_repo::caller_scope::CallerScope::Human,
        )
        .unwrap();
        let source = PathBuf::from(story.story_file_path);
        std::fs::write(&source, "---\ntype: story\nstory_id: SV-1\ntitle: Launch story\nstatus: in-progress\nparent_epic: SV\n---\n").unwrap();
        UsageStore::open(&path)
            .unwrap()
            .register_launch(
                "worker",
                "acme",
                Some("dev"),
                "builder",
                "context-worker",
                Some(cwd.to_str().unwrap()),
            )
            .unwrap();
        let natives = [
            "00000000-0000-4000-8000-000000000036",
            "00000000-0000-4000-8000-000000000037",
        ];
        if usage_first {
            for native in natives {
                ingest_telemetry_for_at(&path, "worker", &cde36_native_export(native)).unwrap();
            }
        }
        write_launch_record_resolving(
            &state,
            "worker",
            "context-worker",
            task_facts(Some("Build")),
            move |record, _| async move {
                Some(StoryLink {
                    story_id: record.story_id.unwrap(),
                    source,
                    org: "acme".into(),
                })
            },
        );
        let mut captured = state
            .task_captures
            .lock()
            .unwrap()
            .get("worker")
            .unwrap()
            .0
            .clone();
        let _ = captured.changed().await;
        if !usage_first {
            for native in natives {
                ingest_telemetry_for_at(&path, "worker", &cde36_native_export(native)).unwrap();
            }
        }
        state.release_for_session("worker");
        // A folder can later be registered for another story. The launch's durable link wins.
        let other_source = dir.path().join("SV-OTHER.md");
        std::fs::write(
            &other_source,
            "---\nstory_id: SV-OTHER\ntitle: Later worktree\n---\n",
        )
        .unwrap();
        let competing_worktree = crate::install::worktree_index::WorktreeStoryRecord::new(
            cwd,
            "acme",
            crate::domain::SourceRepoSlug::new("acme", "app").unwrap(),
            "SV-OTHER",
            other_source,
        );
        for _ in 0..2 {
            let store = UsageStore::open(&path).unwrap();
            let report = store
                .report_with(
                    &Filter {
                        story: Some("SV-1".into()),
                        ..Default::default()
                    },
                    &mut StoryResolver::new(vec![competing_worktree.clone()], Default::default()),
                )
                .unwrap();
            assert_eq!(report.sessions.len(), 2);
            let mut ids: Vec<_> = report
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect();
            ids.sort();
            assert_eq!(ids, natives);
            for session in &report.sessions {
                let story = session.story.as_ref().unwrap();
                assert_eq!(
                    story.title.as_deref(),
                    Some("Launch story"),
                    "usage first: {usage_first}"
                );
                assert_eq!(story.status.as_deref(), Some("in-progress"));
                assert_eq!(story.epic.as_deref(), Some("SV"));
            }
        }
    }
}

#[test]
fn cde36_r1_record_only_story_follows_both_native_sessions_after_clear_and_reopen() {
    use crate::session_usage::{ingest_telemetry_for_at, Filter, StoryResolver, UsageStore};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let store = UsageStore::open(&path).unwrap();
    store
        .register_launch(
            "clear-tab",
            "acme",
            Some("dev"),
            "builder",
            "context-worker",
            None,
        )
        .unwrap();
    let mut record = crate::session_usage::record::SessionRecord::new("clear-tab");
    record.story_id = Some("SV-1".into());
    store.write_session_record(&record).unwrap();
    drop(store);
    let natives = [
        "00000000-0000-4000-8000-000000000036",
        "00000000-0000-4000-8000-000000000037",
    ];
    for native in natives {
        ingest_telemetry_for_at(&path, "clear-tab", &cde36_native_export(native)).unwrap();
    }
    for _ in 0..2 {
        let store = UsageStore::open(&path).unwrap();
        let report = store
            .report_with(
                &Filter {
                    story: Some("SV-1".into()),
                    ..Default::default()
                },
                &mut StoryResolver::default(),
            )
            .unwrap();
        let mut ids: Vec<_> = report
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        ids.sort();
        assert_eq!(
            ids, natives,
            "both native sessions belong to the same durable launch"
        );
        assert_eq!(report.totals.tokens.input, 20);
        let links: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM session_story", [], |r| r.get(0))
            .unwrap();
        assert_eq!(links, 0, "record-only reporting never backfills links");
    }
}

/// `{home}`'s org root for user 42.
fn org_root(home: &Path, org: &str) -> PathBuf {
    crate::context_repo::path_to_org_root(home, &Principal::from(42u64), org).unwrap()
}

/// Write story `id` into `org`'s `team` content, as file `{id}.md`.
fn write_org_story(home: &Path, org: &str, team: &str, id: &str) {
    write_org_story_file(home, org, team, &format!("{id}.md"), id);
}

fn write_org_story_file(home: &Path, org: &str, team: &str, file: &str, id: &str) {
    let content = org_root(home, org).join(team).join("content");
    std::fs::create_dir_all(&content).unwrap();
    std::fs::write(
        content.join(file),
        format!("---\ntype: story\nstory_id: {id}\nsource_repo: acme/app\n---\n# {id}\n"),
    )
    .unwrap();
}

/// A mock app for user 42 with `acme/active` selected: the shared cache is built from that
/// org's stories as written so far, and no other org's index is held yet.
fn two_org_app(
    home: &Path,
) -> (
    tauri::App<tauri::test::MockRuntime>,
    crate::commands::route_to_context::ActiveContext,
) {
    use crate::install::frontmatter_cache::FrontmatterCache;
    use crate::mcp::state::{ActiveTeam, ActiveTeamState};
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(FrontmatterCache::shared(FrontmatterCache::build_from_scan(
        &org_root(home, "acme/active"),
    )));
    app.manage(std::sync::Arc::new(
        crate::install::org_story_resolver::OrgStoryResolver::new(),
    ));
    let selected = ActiveTeamState::default();
    selected.update(ActiveTeam {
        org_slug: "acme/active".into(),
        team_name: "dev".into(),
    });
    app.manage(selected);
    let active = launch_story_context(app.handle(), home.into(), 42);
    (app, active)
}

#[test]
fn a_story_in_a_non_active_org_resolves_there_and_a_later_one_without_a_restart() {
    use crate::context_repo::caller_scope::{CallerScope, MISSING};
    let home = tempfile::tempdir().unwrap();
    write_org_story(home.path(), "acme/active", "dev", "A-1");
    write_org_story(home.path(), "acme/other", "dev", "B-1");
    let (app, active) = two_org_app(home.path());
    let human = CallerScope::Human;

    let found =
        resolve_story_path_in_org_rescanning(app.handle(), &active, "acme/other", "B-1", &human)
            .unwrap();
    assert_eq!(
        PathBuf::from(found.story_file_path),
        org_root(home.path(), "acme/other").join("dev/content/B-1.md")
    );
    // The active org's team does not see the other org's story.
    let active_team = CallerScope::team(
        home.path(),
        &Principal::from(42u64),
        &LaunchTeam {
            org: "acme/active".into(),
            team: "dev".into(),
        },
    );
    assert_eq!(
        resolve_story_path_in_org_for(app.handle(), &active, "acme/active", "B-1", &active_team)
            .unwrap_err(),
        MISSING
    );

    // Written after acme/other's index was built: the plain lookup misses it, the
    // rescanning one finds it.
    write_org_story(home.path(), "acme/other", "dev", "B-2");
    assert!(
        resolve_story_path_in_org_for(app.handle(), &active, "acme/other", "B-2", &human).is_err()
    );
    let late =
        resolve_story_path_in_org_rescanning(app.handle(), &active, "acme/other", "B-2", &human)
            .unwrap();
    assert_eq!(late.team, "dev");
}

/// The orgs [`two_org_app`] syncs, active first.
fn both_orgs() -> Result<Vec<String>, String> {
    Ok(vec!["acme/active".to_string(), "acme/other".to_string()])
}

#[test]
fn with_no_org_a_story_only_in_a_non_active_org_opens_there() {
    // The active org misses with its fallback scan's multi-line text: not a holder.
    let home = tempfile::tempdir().unwrap();
    write_org_story(home.path(), "acme/active", "dev", "A-1");
    write_org_story(home.path(), "acme/other", "web", "B-1");
    let (app, active) = two_org_app(home.path());

    let (org, found) = crate::commands::mcp_workspace::pick_story_org_in(
        app.handle(),
        &active,
        &[],
        "B-1",
        None,
        both_orgs,
    )
    .unwrap();
    assert_eq!((org.as_str(), found.team.as_str()), ("acme/other", "web"));
}

#[test]
fn with_no_org_a_story_found_nowhere_rescans_and_a_late_one_is_found() {
    let home = tempfile::tempdir().unwrap();
    write_org_story(home.path(), "acme/active", "dev", "A-1");
    write_org_story(home.path(), "acme/other", "web", "B-1");
    let (app, active) = two_org_app(home.path());
    let pick = |id: &str| {
        crate::commands::mcp_workspace::pick_story_org_in(
            app.handle(),
            &active,
            &[],
            id,
            None,
            both_orgs,
        )
    };

    // Nowhere: every org misses, the rescan finds nothing, and the caller gets the
    // not-found, not the active org's text counted as a holder.
    assert_eq!(
        pick("Z-9").unwrap_err(),
        "unavailable: no managed story with id 'Z-9'"
    );
    // Written after acme/other's index was built: only the total-miss rescan sees it.
    write_org_story(home.path(), "acme/other", "web", "B-2");
    let (org, found) = pick("B-2").unwrap();
    assert_eq!((org.as_str(), found.team.as_str()), ("acme/other", "web"));
}

#[test]
fn an_agents_story_lookup_never_shows_it_the_unscoped_error() {
    // Two docs in another team carry the id: a person is told it is ambiguous, an agent
    // that cannot see that team is told only that its team has no such story to open.
    use crate::context_repo::caller_scope::CallerScope;
    let home = tempfile::tempdir().unwrap();
    write_org_story(home.path(), "acme/active", "dev", "A-1");
    write_org_story_file(home.path(), "acme/active", "ops", "DUP-1.md", "DUP-1");
    write_org_story_file(home.path(), "acme/active", "ops", "DUP-1-copy.md", "DUP-1");
    let (app, active) = two_org_app(home.path());
    assert!(resolve_story_path_in_org_for(
        app.handle(),
        &active,
        "acme/active",
        "DUP-1",
        &CallerScope::Human
    )
    .unwrap_err()
    .starts_with("unavailable: ambiguous story id 'DUP-1'"));

    let dev = CallerScope::team(
        home.path(),
        &Principal::from(42u64),
        &LaunchTeam {
            org: "acme/active".into(),
            team: "dev".into(),
        },
    );
    let reply = crate::commands::workspace_open::agent_story_visible(
        app.handle(),
        &active,
        &dev,
        "DUP-1",
        None,
    )
    .unwrap_err();
    assert_eq!(
        reply,
        "No story DUP-1 that the dev team can open. Check the story id, and that the \
         story's team is dev or a team dev can launch into."
    );
    for unscoped in [
        "ambiguous",
        "indexed",
        "ops",
        "DUP-1.md",
        "DUP-1-copy",
        "File not found",
    ] {
        assert!(
            !reply.contains(unscoped),
            "{unscoped:?} reached the agent: {reply}"
        );
    }
}

/// `f`'s app with `acme/elsewhere` selected, so the fixture's own org `acme/ctx` is not
/// the active one: the shared cache is the selected org's, and no other org is held yet.
fn unselected_own_org_app(
    f: &crate::context_repo::caller_scope::tests::AppFixture,
) -> crate::commands::route_to_context::ActiveContext {
    use crate::install::frontmatter_cache::FrontmatterCache;
    use crate::mcp::state::{ActiveTeam, ActiveTeamState};
    let home = &f.fixture.home;
    let elsewhere =
        crate::context_repo::path_to_org_root(home, &Principal::from(1u64), "acme/elsewhere")
            .unwrap();
    std::fs::create_dir_all(elsewhere.join("web/content")).unwrap();
    std::fs::write(
        elsewhere.join("web/content/E-1.md"),
        "---\ntype: story\nstory_id: E-1\nsource_repo: acme/app\n---\n# E-1\n",
    )
    .unwrap();
    f.app
        .manage(FrontmatterCache::shared(FrontmatterCache::build_from_scan(
            &elsewhere,
        )));
    f.app.manage(std::sync::Arc::new(
        crate::install::org_story_resolver::OrgStoryResolver::new(),
    ));
    f.app.state::<ActiveTeamState>().update(ActiveTeam {
        org_slug: "acme/elsewhere".into(),
        team_name: "web".into(),
    });
    launch_story_context(f.app.handle(), home.clone(), 1)
}

#[test]
fn a_team_agent_whose_org_is_not_selected_opens_its_story_and_cannot_tell_hidden_from_missing() {
    // The handler's check before an agent's open, as #2839's reporter hit it: a team agent
    // whose own org is not the selected one. Its stories open; an id only a team it cannot
    // see holds gets the same bytes as an id nobody holds. The hidden story is its parent's
    // unshared content: a same-org team outside the chain, like the fixture's sibling, is
    // admitted as another team and so is visible.
    use crate::commands::workspace_open::agent_story_visible;
    use crate::context_repo::caller_scope::tests::AppFixture;
    let story = |id: &str, extra: &str| {
        format!("---\ntype: story\nstory_id: {id}\nsource_repo: acme/app\n{extra}---\n# {id}\n")
    };
    let refused = |hidden: bool| {
        let f = AppFixture::new();
        f.fixture
            .write("child", "content/C-1.md", &story("C-1", ""));
        f.fixture.write(
            "child",
            "content/C-2.md",
            &story("C-2", "parent_epic: SV-E\n"),
        );
        f.fixture
            .write("child", "content/D-1.md", &story("D-1", ""));
        f.fixture
            .write("child", "content/D-1-copy.md", &story("D-1", ""));
        if hidden {
            f.fixture
                .write("parent", "content/H-1.md", &story("H-1", ""));
        }
        let active = unselected_own_org_app(&f);
        let scope = &f.fixture.scope;
        for id in ["C-1", "C-2"] {
            let found = agent_story_visible(f.app.handle(), &active, scope, id, None)
                .unwrap()
                .unwrap();
            assert_eq!(
                PathBuf::from(found.story_file_path),
                f.fixture.path("child", &format!("content/{id}.md"))
            );
        }
        // The lookup's own text reaches the refusal unmapped, which words it for the agent.
        assert!(
            resolve_story_path_in_org_as(f.app.handle(), &active, "acme/ctx", "D-1", scope)
                .unwrap_err()
                .starts_with("unavailable: ambiguous story id 'D-1'")
        );
        assert_eq!(
            agent_story_visible(f.app.handle(), &active, scope, "D-1", None).unwrap_err(),
            "No story D-1 that the child team can open. Check the story id, and that the \
             story's team is child or a team child can launch into."
        );
        // A blank id is left to the open, which says it needs one.
        assert!(
            agent_story_visible(f.app.handle(), &active, scope, "  ", None)
                .unwrap()
                .is_none()
        );
        agent_story_visible(f.app.handle(), &active, scope, "H-1", None).unwrap_err()
    };

    let hidden = refused(true);
    assert_eq!(hidden, refused(false));
    assert_eq!(
        hidden,
        "No story H-1 that the child team can open. Check the story id, and that the \
         story's team is child or a team child can launch into."
    );
}
