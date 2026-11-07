// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::{tests::AppFixture, CallerScope, MISSING};
use crate::domain::Principal;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Copy, Debug)]
enum Consumer {
    Remove,
    Pane,
    AvatarAdd,
    AvatarShare,
    Adopt,
    Route,
    EpicRepos,
    Starter,
    RegistryStarter,
    Metadata,
    Fields,
    Draft,
    FileCreate,
    FileDelete,
    ContextCopy,
    ContextMove,
}

fn reply<T: serde::Serialize>(result: Result<T, String>) -> Value {
    match result {
        Ok(value) => serde_json::to_value(value).unwrap(),
        Err(error) => json!({"error": error}),
    }
}

fn tree(path: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry
                    .path()
                    .strip_prefix(path)
                    .unwrap()
                    .display()
                    .to_string(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

// Exercise each target-reading writer at its consumer seam. Compare its filesystem
// effects with the same baseline operation, including collisions and CAS refusals.
#[test]
fn mutation_census_masks_all_outcomes_without_changing_baseline_writes() {
    use Consumer::*;
    for consumer in [
        Remove,
        Pane,
        AvatarAdd,
        AvatarShare,
        Adopt,
        Route,
        EpicRepos,
        Starter,
        RegistryStarter,
        Metadata,
        Fields,
        Draft,
        FileCreate,
        FileDelete,
        ContextCopy,
        ContextMove,
    ] {
        for admitted in [false, true] {
            let mut hidden_reply = None;
            for state in ["existing", "different", "equal", "absent"] {
                let mut baseline_effect = None;
                for baseline in [true, false] {
                    let f = AppFixture::new();
                    let me = Principal::from(1u64);
                    use tauri::Listener;
                    let notified = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                    let observed = notified.clone();
                    f.app.listen(crate::avatars::AVATARS_CHANGED, move |_| {
                        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    });
                    let notify = || notify_avatar_change(f.app.handle());
                    let logical = match consumer {
                        Pane => "panes/matrix.html",
                        AvatarAdd | AvatarShare => "content/avatars/matrix/pack.yaml",
                        _ => "content/matrix.md",
                    };
                    f.fixture.write("parent", "config/sharing.yml", if admitted {
                        "share: [content/shared.md, content/matrix.md, panes/matrix.html, content/avatars/matrix, 'content/avatars/matrix/**']\n"
                    } else {"share: []\n"});
                    let source =
                        "---\ntype: epic\nrepos: [acme/api]\nstatus: todo\n---\n# Matrix\n";
                    let different =
                        "---\ntype: epic\nrepos: [other/api]\nstatus: todo\n---\n# Different\n";
                    let before = if state == "different" {
                        different
                    } else {
                        source
                    };
                    let dest = f.fixture.path("parent", logical);
                    let avatar_source = f.fixture.write("child", "content/input.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\"><rect width=\"64\" height=\"64\"/></svg>");
                    let avatar_params = || {
                        serde_json::from_value(json!({"pack":"matrix","id":"face","file":avatar_source,"label":"Face"})).unwrap()
                    };
                    if matches!(consumer, AvatarAdd | AvatarShare) {
                        if state != "absent" {
                            let label = if state == "different" { "Old" } else { "Face" };
                            let params = serde_json::from_value(json!({"pack":"matrix","id":"face","file":avatar_source,"label":label})).unwrap();
                            let dirs = crate::avatars::store::avatars_dir_for_add(
                                &f.fixture.home,
                                &me,
                                "acme/ctx",
                                "parent",
                                crate::avatars::PackScope::Team,
                            );
                            assert!(
                                serde_json::to_value(avatar_tools::add(
                                    crate::avatars::PackScope::Team,
                                    dirs,
                                    params
                                ))
                                .unwrap()["ok"]
                                    == true
                            );
                        }
                        let personal = crate::avatars::store::avatars_dir_for_add(
                            &f.fixture.home,
                            &me,
                            "acme/ctx",
                            "parent",
                            crate::avatars::PackScope::Personal,
                        );
                        assert!(
                            serde_json::to_value(avatar_tools::add(
                                crate::avatars::PackScope::Personal,
                                personal,
                                avatar_params()
                            ))
                            .unwrap()["ok"]
                                == true
                        );
                    } else if state != "absent" {
                        f.fixture.write("parent", logical, before);
                    }
                    f.fixture.write("child", "content/matrix.md", source);
                    let caller = if baseline {
                        CallerScope::Human
                    } else {
                        CallerScope::team(
                            &f.fixture.home,
                            &me,
                            &commands::dashboard_launch::LaunchTeam {
                                org: "acme/ctx".into(),
                                team: "child".into(),
                            },
                        )
                    };
                    let result = match consumer {
                        Remove => reply(commands::remove_from_context::remove_from_context_for(
                            &f.fixture.home,
                            Some("acme/ctx"),
                            &commands::remove_from_context::RemoveFromContextParams {
                                context:
                                    commands::move_between_contexts::ContextEndpoint::TeamAll {
                                        org_slug: "acme/ctx".into(),
                                        team_name: "parent".into(),
                                    },
                                logical_name: "matrix.md".into(),
                                user_id: 1,
                            },
                            &caller,
                        )),
                        Pane => reply(pane_tools::write_for(
                            &f.fixture.home,
                            &me,
                            &crate::panes::PaneTarget::Team {
                                org: "acme/ctx".into(),
                                team: "parent".into(),
                            },
                            "matrix",
                            source,
                            &caller,
                        )),
                        AvatarAdd => serde_json::to_value(avatar_tools::add_for(
                            crate::avatars::PackScope::Team,
                            Ok(dest.parent().unwrap().parent().unwrap().into()),
                            avatar_params(),
                            &caller,
                            &notify,
                        ))
                        .unwrap(),
                        AvatarShare => serde_json::to_value(avatar_tools::share_for(
                            &f.fixture.home,
                            &me,
                            "acme/ctx",
                            "parent",
                            avatar_tools::AvatarShareParams {
                                pack: "matrix".into(),
                            },
                            &caller,
                            &notify,
                        ))
                        .unwrap(),
                        Adopt => {
                            let dispatcher = crate::content_engine::dispatcher::Dispatcher::new(
                                1, 100, 1000, 500,
                            );
                            let ctx = ActiveContext::for_team(
                                f.fixture.home.clone(),
                                1,
                                "acme/ctx".into(),
                                "parent".into(),
                            );
                            let params = SenseSyncAdoptParams {
                                code_repo_path: f.fixture.temp.path().into(),
                                code_repo_slug: crate::domain::SourceRepoSlug::new("acme", "api")
                                    .unwrap(),
                                logical_name: "matrix.md".into(),
                                target_layer: crate::content_engine::LayerId::TeamProjection,
                                content: source.into(),
                                expected_layer_hash: Some(
                                    blake3::hash(if state == "equal" {
                                        source.as_bytes()
                                    } else {
                                        b"stale"
                                    })
                                    .to_hex()
                                    .to_string(),
                                ),
                            };
                            reply(
                                tauri::async_runtime::block_on(
                                    super::super::context_tools::adopt_for(
                                        ctx,
                                        params,
                                        &caller,
                                        || Ok(dispatcher.suppression_handle()),
                                    ),
                                )
                                .map_err(|e| e.message.to_string()),
                            )
                        }
                        Route => {
                            let workspace = f.fixture.temp.path().join("workspace");
                            let repo = git2::Repository::init(&workspace).unwrap();
                            repo.remote("origin", "https://github.com/acme/api.git")
                                .unwrap();
                            std::fs::write(workspace.join("matrix.md"), source).unwrap();
                            reply(
                                crate::agents::prepare::publish::route_for(
                                    &f.fixture.home,
                                    &me,
                                    &workspace,
                                    &crate::agents::ManifestTarget {
                                        context_root: None,
                                        org: "acme/ctx".into(),
                                        team: "parent".into(),
                                        repo: Some(
                                            crate::domain::SourceRepoSlug::new("acme", "api")
                                                .unwrap(),
                                        ),
                                    },
                                    "matrix.md",
                                    "matrix",
                                    None,
                                    &caller,
                                )
                                .map(|out| format!("{out:?}")),
                            )
                        }
                        EpicRepos => {
                            let writes = std::cell::Cell::new(0);
                            let out = commands::pipeline_scanner::set_epic_repos_for(
                                &dest,
                                &["acme/api".into()],
                                &caller,
                                |_| writes.set(writes.get() + 1),
                            );
                            assert_eq!(writes.get(), usize::from(state != "absent"));
                            reply(out)
                        }
                        Starter | RegistryStarter => {
                            let result = if matches!(consumer, Starter) {
                                tauri::async_runtime::block_on(super::super::team_tools::set_team_starter_page_for(f.app.handle(),&f.fixture.home,&me,None,serde_json::from_value(json!({"org":"acme/ctx","team":"parent","path":"matrix.md"})).unwrap(),&caller)).map(|out|serde_json::to_value(out).unwrap())
                            } else {
                                tauri::async_runtime::block_on(
                                    super::super::local_setup::dispatch_for(
                                        f.app.handle(),
                                        &f.fixture.home,
                                        &me,
                                        "commit_registry_edit_local",
                                        json!({"orgSlug":"acme/ctx","edit":{"type":"SetTeamLandingPage","team_name":"parent","landing_page":"matrix.md"}}),
                                        &caller,
                                    ),
                                )
                            };
                            reply(result)
                        }
                        Metadata | Fields | Draft => {
                            f.app.manage(f.fixture.service());
                            let page = f.fixture.write(
                                "child",
                                "panes/page.html",
                                "<title>Caller</title>",
                            );
                            let frame = f.frame(&page, "open");
                            let (page, frame) = if baseline {
                                (None, None)
                            } else {
                                (Some(page.display().to_string()), Some(frame.frame_id))
                            };
                            let result = if matches!(consumer, Metadata) {
                                tauri::async_runtime::block_on(
                                    commands::frontmatter_editor::apply_metadata_edit(
                                        f.app.handle().clone(),
                                        dest.display().to_string(),
                                        vec![
                                            commands::frontmatter_editor::MetadataEdit::SetField {
                                                field: "status".into(),
                                                value: "done".into(),
                                            },
                                        ],
                                        page,
                                        frame,
                                    ),
                                )
                            } else if matches!(consumer, Fields) {
                                tauri::async_runtime::block_on(
                                    commands::frontmatter_editor::update_frontmatter(
                                        f.app.handle().clone(),
                                        dest.display().to_string(),
                                        json!({"status":"done"}).as_object().unwrap().clone(),
                                        page,
                                        frame,
                                    ),
                                )
                            } else {
                                tauri::async_runtime::block_on(commands::files::create_file(
                                    f.app.handle().clone(),
                                    dest.display().to_string(),
                                    source.into(),
                                    page,
                                    frame,
                                ))
                            };
                            reply(result)
                        }
                        FileCreate | FileDelete => reply(tauri::async_runtime::block_on(
                            commands::file_surface::call_for(
                                f.app.handle().clone(),
                                if matches!(consumer, FileCreate) {
                                    "create"
                                } else {
                                    "delete"
                                },
                                json!({"path":dest,"content":source}),
                                None,
                                caller,
                            ),
                        )),
                        ContextCopy | ContextMove => {
                            reply(commands::move_between_contexts::move_between_contexts_for(
                                &f.fixture.home,
                                Some("acme/ctx"),
                                &commands::move_between_contexts::MoveBetweenContextsParams {
                                    from:
                                        commands::move_between_contexts::ContextEndpoint::TeamAll {
                                            org_slug: "acme/ctx".into(),
                                            team_name: "child".into(),
                                        },
                                    to: commands::move_between_contexts::ContextEndpoint::TeamAll {
                                        org_slug: "acme/ctx".into(),
                                        team_name: "parent".into(),
                                    },
                                    logical_name: "matrix.md".into(),
                                    to_logical_name: None,
                                    user_id: 1,
                                    force: false,
                                    retain_source: matches!(consumer, ContextCopy),
                                },
                                None,
                                &caller,
                            ))
                        }
                    };
                    if matches!(consumer, AvatarAdd | AvatarShare) {
                        let expected =
                            usize::from(matches!(consumer, AvatarAdd) || state == "absent");
                        assert_eq!(
                            notified.load(std::sync::atomic::Ordering::SeqCst),
                            expected,
                            "{consumer:?}/{state}: raw successful writes notify before masking"
                        );
                    }
                    let effects = (
                        tree(&f.fixture.path("parent", "content")),
                        tree(&f.fixture.path("parent", "panes")),
                        tree(&f.fixture.path("child", "content")),
                        std::fs::read(f.fixture.root.join("teams.yml")).unwrap(),
                        notified.load(std::sync::atomic::Ordering::SeqCst),
                    );
                    if baseline {
                        baseline_effect = Some(effects);
                    } else {
                        assert_eq!(
                            &effects,
                            baseline_effect.as_ref().unwrap(),
                            "{consumer:?}/{state}/{admitted}: filesystem effects"
                        );
                        if admitted {
                            assert!(
                                !result.to_string().contains(MISSING),
                                "{consumer:?}/{state}: {result}"
                            );
                        } else {
                            assert!(
                                result.to_string().contains(MISSING),
                                "{consumer:?}/{state}: {result}"
                            );
                            if let Some(previous) = &hidden_reply {
                                assert_eq!(&result, previous, "{consumer:?}/{state}");
                            } else {
                                hidden_reply = Some(result);
                            }
                        }
                    }
                }
            }
        }
    }
}
