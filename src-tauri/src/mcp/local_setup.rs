// SPDX-License-Identifier: MIT
//! Bounded local setup dispatch shared by the MCP handlers and MockRuntime tests.
use crate::commands;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::Path;
use tauri::Manager;

#[derive(Debug, serde::Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateLocalOrganizationParams {
    /// Nonempty organization name; identity becomes local/<sanitized-name>.
    pub display_name: String,
    /// Absolute NEW folder beneath an existing parent. Existing folders are refused.
    pub destination_path: String,
}

#[derive(Debug, serde::Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalRegistryEditParams {
    pub org_slug: String,
    /// Existing registry edit, e.g. {"type":"CreateTeam","team_name":"engineering"}.
    /// Creator identity is supplied by the selected local profile.
    pub edit: crate::context_repo::RegistryEdit,
}

#[derive(Debug, serde::Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachSourceCheckoutParams {
    /// Absolute existing source Git checkout root. Its origin is read locally.
    pub repo_path: String,
    /// Line-attribution capture for this repo: `on`, `off`, or `auto` (the default: on).
    /// Omit to leave the current choice.
    #[serde(default)]
    pub attribution: Option<crate::attribution::prefs::AttributionChoice>,
    /// Where this repo's attribution notes go: `local` (the default: `refs/notes/sense`, kept
    /// on this machine) or `push` (to origin with the branch; refused as `no_remote` without
    /// an origin). Omit to leave the current choice.
    #[serde(default)]
    pub notes: Option<crate::attribution::prefs::NotesMode>,
    /// Deprecated and ignored: capture no longer depends on whether the repo is private.
    /// Accepted for one release so existing callers keep working.
    #[serde(default)]
    pub private: Option<bool>,
}

pub(crate) fn admit(tool: &str) -> Result<(), String> {
    commands::interactive_gate::InteractiveGate::admit(tool).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) async fn dispatch_for<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    principal: &crate::domain::Principal,
    tool: &str,
    args: serde_json::Value,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<serde_json::Value, String> {
    let readable = matches!(scope, crate::context_repo::caller_scope::CallerScope::Human)
        || if tool == "commit_registry_edit_local" {
            serde_json::from_value::<LocalRegistryEditParams>(args.clone())
                .ok()
                .is_none_or(|params| match params.edit {
                    crate::context_repo::RegistryEdit::SetTeamLandingPage {
                        team_name,
                        landing_page: Some(page),
                    } => scope.allows_team_content(
                        home,
                        principal,
                        &params.org_slug,
                        &team_name,
                        &page,
                    ),
                    _ => true,
                })
        } else {
            true
        };
    let result = dispatch(app, home, tool, args).await;
    crate::context_repo::caller_scope::readback(readable, result)
}

/// Caller identity is resolved here, never accepted as a tool argument.
pub(crate) async fn dispatch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &Path,
    tool: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    admit(tool)?;
    let principal = commands::local_profile::active(app)?.principal;
    match tool {
        "create_local_organization" => {
            let params: CreateLocalOrganizationParams =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            let result = commands::local_organization::create_for_profile(
                app,
                home.to_path_buf(),
                params.display_name,
                params.destination_path,
                principal.to_string(),
                false,
            )
            .await?;
            serde_json::to_value(result).map_err(|e| e.to_string())
        }
        "commit_registry_edit_local" => {
            let params: LocalRegistryEditParams =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            let result = commands::registry_local::commit_registry_edit_for_profile(
                app,
                home,
                &principal,
                &params.org_slug,
                params.edit,
            )
            .await?;
            serde_json::to_value(result).map_err(|e| e.to_string())
        }
        "save_source_repo_attachment" => {
            let params: AttachSourceCheckoutParams =
                serde_json::from_value(args).map_err(|e| e.to_string())?;
            let coord = app.state::<std::sync::Arc<crate::content_engine::sync::WaveCoordinator>>();
            let active = app.state::<super::state::ActiveTeamState>();
            // `private` is deprecated: accepted and ignored.
            let (repo_path, attribution, notes) =
                (params.repo_path.clone(), params.attribution, params.notes);
            // Refused before anything is saved, so no other choice in the request is lost.
            crate::attribution::prefs::check_notes(&repo_path, notes)?;
            commands::attachments::save_source_for_profile(
                app,
                home,
                &principal,
                params.repo_path,
                None,
                coord.inner(),
                active.inner(),
            )
            .await?;
            if attribution.is_some() || notes.is_some() {
                crate::attribution::prefs::set_for_checkout(
                    home,
                    &principal,
                    &repo_path,
                    attribution,
                    notes,
                )?;
            }
            let mut result = serde_json::Map::new();
            if let Some(mode) = notes {
                // Saved; applying it now means the next push honours it with no relaunch.
                let config_home = home.to_path_buf();
                let applied = tokio::task::spawn_blocking(move || {
                    crate::attribution::launch::reapply_config(&config_home)
                })
                .await
                .map_err(|e| e.to_string())?;
                let mut value = serde_json::to_value(applied).map_err(|e| e.to_string())?;
                value["mode"] = serde_json::to_value(mode).map_err(|e| e.to_string())?;
                result.insert("notes".into(), value);
            }
            if matches!(
                attribution,
                Some(crate::attribution::prefs::AttributionChoice::On)
            ) {
                let install_home = home.to_path_buf();
                let status = tokio::task::spawn_blocking(move || {
                    crate::attribution::install::ensure_installed(
                        &install_home,
                        std::env::var_os("PATH").as_deref(),
                        crate::attribution::install::Trigger::OnDemand,
                    )
                })
                .await
                .map_err(|e| e.to_string())?;
                result.insert(
                    "attribution".into(),
                    serde_json::to_value(status).map_err(|e| e.to_string())?,
                );
            }
            Ok(if serde_json::Map::is_empty(&result) {
                serde_json::Value::Null
            } else {
                serde_json::Value::Object(result)
            })
        }
        _ => Err(format!("Unknown local setup tool: {tool}")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::domain::Principal;
    use tauri::Listener;

    pub(crate) fn app(home: &Path) -> tauri::App<tauri::test::MockRuntime> {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(commands::local_profile::LocalProfileState::load(home).unwrap());
        app.manage(commands::watch::WatcherState::default());
        app.manage(std::sync::Arc::new(
            crate::content_engine::sync::WaveCoordinator::default(),
        ));
        app.manage(super::super::state::ActiveTeamState::default());
        app
    }

    #[tokio::test]
    async fn attribution_on_dispatch_waits_for_the_injected_installer() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let app = app(&home);
        let repo = temp.path().join("source");
        git2::Repository::init(&repo)
            .unwrap()
            .remote("origin", "https://github.com/example/app.git")
            .unwrap();
        let count = crate::attribution::install::tests::register(&home);
        let result = dispatch(
            app.handle(),
            &home,
            "save_source_repo_attachment",
            serde_json::json!({"repoPath":repo, "attribution":"on"}),
        )
        .await
        .unwrap();
        assert_eq!(result, serde_json::json!({"attribution":null}));
        assert!(home.join("bin/git-ai").is_file());
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn attribution_on_dispatch_releases_a_user_pinned_native_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let app = app(&home);
        let repo = temp.path().join("source");
        git2::Repository::init(&repo)
            .unwrap()
            .remote("origin", "https://github.com/example/app.git")
            .unwrap();
        crate::attribution::install::tests::register_restored_bundle(&home);
        let before =
            super::super::setup_tools::describe_setup_at(app.handle(), home.clone(), 1, None)
                .await
                .unwrap();
        let before = super::super::setup_tools::ToolEnvelope::Ok(before);
        let before = super::super::handler::envelope_json(&before).unwrap();
        let before: serde_json::Value =
            serde_json::from_str(&before.content[0].as_text().unwrap().text).unwrap();
        assert_eq!(
            before["data"]["attribution"]["status"]["reason"],
            "user_pinned"
        );
        let result = dispatch(
            app.handle(),
            &home,
            "save_source_repo_attachment",
            serde_json::json!({"repoPath":repo, "attribution":"on"}),
        )
        .await
        .unwrap();
        assert_eq!(result, serde_json::json!({"attribution":null}));
        assert!(!home.join("bin/user-restored").exists());
        let after =
            super::super::setup_tools::describe_setup_at(app.handle(), home.clone(), 1, None)
                .await
                .unwrap();
        let after = super::super::setup_tools::ToolEnvelope::Ok(after);
        let after = super::super::handler::envelope_json(&after).unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&after.content[0].as_text().unwrap().text).unwrap();
        let attribution = &after["data"]["attribution"];
        assert_eq!(
            attribution["gitAiVersion"],
            crate::attribution::install::PIN.expected_version
        );
        assert_ne!(attribution["status"]["reason"], "user_pinned");
        assert!(!attribution["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().starts_with("user_pinned:")));
        // An app restart must not re-pin the successfully reinstalled bundled bytes.
        crate::attribution::install::tests::restart_registered_bundle(&home);
        assert!(!home.join("bin/user-restored").exists());
    }

    #[test]
    fn upstream_setup_mutations_are_interactive() {
        for tool in [
            "create_team",
            "rename_team",
            "archive_team",
            "delete_team",
            "set_team_parent",
            "set_team_starter_page",
            "rename_workspace",
            "add_source_repo",
            "attach_source_repo_path",
            "remove_source_repo",
        ] {
            assert!(commands::interactive_gate::INTERACTIVE_VERBS.contains(&tool));
        }
    }

    #[tokio::test]
    async fn production_dispatch_creates_team_and_source_without_active_team_or_provider() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let app = app(&home);
        let root = temp.path().join("context");
        let created = dispatch(
            app.handle(),
            &home,
            "create_local_organization",
            serde_json::json!({"displayName":"Local Example", "destinationPath":root}),
        )
        .await
        .unwrap();
        assert_eq!(created["clone_outcome"], "created");
        assert!(created["loaded_org"]["file_errors"]
            .as_array()
            .unwrap()
            .is_empty());
        for (name, forged) in [("engineering", false), ("design", true)] {
            let edit = if forged {
                serde_json::json!({"type":"CreateTeam", "team_name":name, "creator_id":42, "creator_username":"Forged", "creator_display_name":"Forged"})
            } else {
                serde_json::json!({"type":"CreateTeam", "team_name":name})
            };
            dispatch(
                app.handle(),
                &home,
                "commit_registry_edit_local",
                serde_json::json!({"orgSlug":"local-example", "edit":edit}),
            )
            .await
            .unwrap();
        }
        let loaded = crate::domain::load_org("local-example", &root, None);
        assert!(loaded.file_errors.is_empty(), "{:?}", loaded.file_errors);
        let org = loaded.org.unwrap();
        assert_eq!(org.teams.len(), 2);
        assert_eq!(org.people.len(), 1);
        assert!(!std::fs::read_to_string(root.join("people.yml"))
            .unwrap()
            .contains("Forged"));
        assert!(std::fs::read_to_string(root.join("people.yml"))
            .unwrap()
            .contains("Local profile"));
        let repo = temp.path().join("source");
        git2::Repository::init(&repo)
            .unwrap()
            .remote("origin", "https://github.com/example/app.git")
            .unwrap();
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = events.clone();
        app.listen(
            commands::attachments::SOURCE_ATTACHMENTS_CHANGED_EVENT,
            move |event| {
                sink.lock()
                    .unwrap()
                    .push(serde_json::from_str::<serde_json::Value>(event.payload()).unwrap());
            },
        );
        dispatch(
            app.handle(),
            &home,
            "save_source_repo_attachment",
            serde_json::json!({"repoPath":repo}),
        )
        .await
        .unwrap();
        assert_eq!(
            events.lock().unwrap()[0]["principal"],
            Principal::local_guest().as_str()
        );
        assert!(app
            .state::<super::super::state::ActiveTeamState>()
            .get()
            .is_none());
        let described =
            super::super::setup_tools::describe_orgs(&home, &Principal::local_guest()).unwrap();
        assert_eq!(described[0].teams.len(), 2);
    }

    #[tokio::test]
    async fn production_dispatch_refuses_foreign_principal_arguments_before_writes() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let app = app(&home);
        let root = temp.path().join("forbidden");
        assert!(dispatch(
            app.handle(),
            &home,
            "create_local_organization",
            serde_json::json!({"displayName":"Forbidden", "destinationPath":root, "principal":"42"})
        )
        .await
        .is_err());
        assert!(!root.exists());
        assert!(
            crate::context_repo::context_attachments::list(&home, &Principal::local_guest())
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn actual_creation_and_registry_adapters_refuse_stale_profile_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        std::fs::create_dir_all(home.join("users/42")).unwrap();
        std::fs::create_dir_all(home.join("users/43")).unwrap();
        let app = app(&home);
        app.state::<commands::local_profile::LocalProfileState>()
            .select("42", Some("First".into()))
            .unwrap();
        let root = temp.path().join("stale");
        let pending = commands::local_organization::create_for_profile(
            app.handle(),
            home.clone(),
            "Stale".into(),
            root.to_string_lossy().into_owned(),
            "42".into(),
            false,
        );
        app.state::<commands::local_profile::LocalProfileState>()
            .select("43", Some("Second".into()))
            .unwrap();
        assert!(pending.await.unwrap_err().contains("profile changed"));
        let edit =
            serde_json::from_value(serde_json::json!({"type":"CreateTeam","team_name":"stale"}))
                .unwrap();
        assert!(commands::registry_local::commit_registry_edit_for_profile(
            app.handle(),
            &home,
            &Principal::from(42u64),
            "local/stale",
            edit
        )
        .await
        .unwrap_err()
        .contains("profile changed"));
        assert!(!root.exists());
        for id in [42u64, 43u64] {
            assert!(
                crate::context_repo::context_attachments::list(&home, &Principal::from(id))
                    .unwrap()
                    .is_empty()
            );
        }
    }

    /// Attaching through the MCP entry fetches origin once, in the background.
    #[tokio::test]
    async fn attaching_a_checkout_fetches_its_origin_in_the_background() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let app = app(&home);
        let fixture_home = temp.path().join("fixture");
        let trail = crate::attribution::history::tests::Trail::new(&fixture_home, "main", None);
        let new_head = trail.advance_origin();
        dispatch(
            app.handle(),
            &home,
            "save_source_repo_attachment",
            serde_json::json!({"repoPath": trail.clone}),
        )
        .await
        .unwrap();
        let source = crate::attribution::history::HistorySource::at(&trail.clone).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while source.history_as_of(&home)["originHead"] != new_head.as_str()
            || !source.history_as_of(&home)["fetchedAt"].is_i64()
        {
            assert!(std::time::Instant::now() < deadline, "the attach fetch never landed");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(source.history_as_of(&home)["fetchedAt"].is_i64());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_alias_lookup_and_repeat_attachment_preserve_metadata_and_reject_conflicts() {
        use crate::context_repo::attachments_io::{
            read_attachments, resolve_by_repo_path, write_attachments_atomic,
        };
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let app = app(&home);
        let root = temp.path().join("source");
        let repo = git2::Repository::init(&root).unwrap();
        repo.remote("origin", "https://github.com/example/app.git")
            .unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let path =
            crate::context_repo::path_to_user_attachments_file(&home, &Principal::local_guest());
        let mut attachment = commands::attachments::existing_checkout_attachment(&root).unwrap();
        attachment.repo_path = alias.to_string_lossy().into_owned();
        attachment.managed = true;
        attachment.attached_at = Some("2020-01-01T00:00:00Z".parse().unwrap());
        attachment.tracking_prompt_dismissed_at = attachment.attached_at;
        write_attachments_atomic(
            &path,
            &crate::domain::Attachments {
                attachments: vec![attachment.clone()],
            },
        )
        .unwrap();
        for selected in [&root, &alias] {
            dispatch(
                app.handle(),
                &home,
                "save_source_repo_attachment",
                serde_json::json!({"repoPath":selected}),
            )
            .await
            .unwrap();
        }
        let data = read_attachments(&path).unwrap();
        assert_eq!(data.attachments.len(), 1);
        assert_eq!(
            resolve_by_repo_path(&data.attachments, alias.to_str().unwrap()).unwrap(),
            Some(0)
        );
        assert_eq!(data.attachments[0].attached_at, attachment.attached_at);
        assert_eq!(
            data.attachments[0].tracking_prompt_dismissed_at,
            attachment.tracking_prompt_dismissed_at
        );
        assert!(data.attachments[0].managed);
        assert_eq!(data.attachments[0].remote_url, attachment.remote_url);
        let mut conflicting = data;
        attachment.remote_url = Some("https://github.com/other/repo.git".into());
        conflicting.attachments.push(attachment);
        write_attachments_atomic(&path, &conflicting).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(dispatch(
            app.handle(),
            &home,
            "save_source_repo_attachment",
            serde_json::json!({"repoPath":root})
        )
        .await
        .unwrap_err()
        .contains("Multiple source attachment"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    #[test]
    fn queued_creation_rechecks_profile_after_initial_admission_before_any_publication() {
        use std::future::Future;
        use std::task::Poll;
        use std::time::Duration;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("state");
            for id in [42, 43] {
                std::fs::create_dir_all(home.join(format!("users/{id}"))).unwrap();
            }
            let app = app(&home);
            app.state::<commands::local_profile::LocalProfileState>()
                .select("42", Some("First".into()))
                .unwrap();
            let root = temp.path().join("queued-creation");
            let before: Vec<_> = [42u64, 43u64]
                .into_iter()
                .map(|id| {
                    let path = crate::context_repo::context_attachments::registry_path(
                        &home,
                        &Principal::from(id),
                    );
                    (path.clone(), std::fs::read(path).ok())
                })
                .collect();

            // Occupy this runtime's only blocking worker. The actual adapter can
            // pass initial admission, but its publication closure cannot run yet.
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(10))
                    .expect("release queued worker");
            });
            tokio::time::timeout(Duration::from_secs(5), ready)
                .await
                .unwrap()
                .unwrap();
            let mut pending = Box::pin(commands::local_organization::create_for_profile(
                app.handle(),
                home.clone(),
                "Queued".into(),
                root.to_string_lossy().into_owned(),
                "42".into(),
                false,
            ));
            std::future::poll_fn(|cx| match pending.as_mut().poll(cx) {
                Poll::Pending => Poll::Ready(()),
                Poll::Ready(result) => {
                    panic!("creation must queue after initial admission: {result:?}")
                }
            })
            .await;
            // The future was genuinely polled: its first await is await_blocking,
            // after the initial active-principal comparison has succeeded.
            app.state::<commands::local_profile::LocalProfileState>()
                .select("43", Some("Second".into()))
                .unwrap();
            release.send(()).unwrap();
            blocker.await.unwrap();
            let error = tokio::time::timeout(Duration::from_secs(5), pending)
                .await
                .unwrap()
                .unwrap_err();
            assert!(error.contains("profile changed"), "{error}");
            assert!(!root.exists());
            assert!(!std::fs::read_dir(temp.path()).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".codesense-create-")));
            for (path, bytes) in before {
                assert_eq!(std::fs::read(path).ok(), bytes);
            }
        });
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_dispatch_rejects_control_characters_in_inspected_path_or_origin_without_writes()
    {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let app = app(&home);
        let path =
            crate::context_repo::path_to_user_attachments_file(&home, &Principal::local_guest());
        let valid = temp.path().join("valid");
        git2::Repository::init(&valid).unwrap();
        dispatch(
            app.handle(),
            &home,
            "save_source_repo_attachment",
            serde_json::json!({"repoPath":valid}),
        )
        .await
        .unwrap();
        let before = std::fs::read(&path).unwrap();

        let invalid_path = temp.path().join("invalid\ncheckout");
        git2::Repository::init(&invalid_path).unwrap();
        let path_error = dispatch(
            app.handle(),
            &home,
            "save_source_repo_attachment",
            serde_json::json!({"repoPath":invalid_path}),
        )
        .await
        .unwrap_err();
        assert!(path_error.contains("repo_path"), "{path_error}");
        assert!(path_error.contains("control character"), "{path_error}");
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let invalid_origin = temp.path().join("invalid-origin");
        let repo = git2::Repository::init(&invalid_origin).unwrap();
        repo.config()
            .unwrap()
            .set_str("remote.origin.url", "https://github.com/example/app\n.git")
            .unwrap();
        let origin_error = dispatch(
            app.handle(),
            &home,
            "save_source_repo_attachment",
            serde_json::json!({"repoPath":invalid_origin}),
        )
        .await
        .unwrap_err();
        assert!(origin_error.contains("remote_url"), "{origin_error}");
        assert!(origin_error.contains("control character"), "{origin_error}");
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}
