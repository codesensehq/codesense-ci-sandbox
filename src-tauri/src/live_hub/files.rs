// SPDX-License-Identifier: MIT
//! Cached directory views driven by the existing context-file-change event.
use crate::commands::file_surface::{self, ContentAccess, ListFiles};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use tauri::{Listener, Manager};

#[derive(Clone)]
pub(super) struct FileView {
    pub access: ContentAccess,
    pub args: ListFiles,
    pub team_key: String,
    /// Team-relative coordinates stay stable across content layers.
    pub root: PathBuf,
    pub rows: BTreeMap<String, Value>,
    pub folder_rows: BTreeMap<String, BTreeMap<String, Value>>,
}
#[derive(Default)]
pub(super) struct Files {
    pub views: BTreeMap<String, FileView>,
    pub pending: BTreeSet<PathBuf>,
    pub agents_changed: bool,
    pub inherited: BTreeSet<PathBuf>,
}
fn rows(value: Value) -> BTreeMap<String, Value> {
    value["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| (r["relativePath"].as_str().unwrap().into(), r.clone()))
        .collect()
}
fn folder_rows(
    access: &ContentAccess,
    entries: &BTreeMap<String, Value>,
) -> BTreeMap<String, BTreeMap<String, Value>> {
    entries
        .values()
        .filter(|row| file_surface::is_folder(row))
        .filter_map(|row| {
            let folder = row["relativePath"].as_str()?;
            Some((
                folder.to_owned(),
                rows(json!({"rows":access.list_rows(Some(folder)).unwrap_or_default()})),
            ))
        })
        .collect()
}

fn key(base: &str, args: &ListFiles, access: &ContentAccess) -> String {
    format!(
        "{base}|files:{}",
        json!([
            args.scope.as_deref().unwrap_or("team"),
            args.team,
            args.story_id,
            args.agent_id,
            args.dir,
            access.own,
            access.caller.cache_key()
        ])
    )
}

fn is_installed_dir<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    base: &str,
    args: &ListFiles,
) -> bool {
    app.try_state::<super::LiveHub>().is_some_and(|hub| {
        hub.model
            .lock()
            .unwrap()
            .nav_files
            .get(base)
            .is_some_and(|native| native.installed_dir(args.dir.as_deref().unwrap_or("")))
    })
}

/// One listing for subscriptions and direct `files.list`: the raw rows (an installed folder has
/// no physical directory) and those rows with the native overlay applied.
pub(crate) async fn list_native<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    access: &ContentAccess,
    args: &ListFiles,
    base: &str,
) -> Result<(Value, Vec<Value>), String> {
    let values = if is_installed_dir(app, base, args) {
        json!({"rows":[]})
    } else {
        file_surface::list_with_access(app.clone(), access.clone(), args.clone(), base).await?
    };
    let raw = values["rows"].as_array().cloned().unwrap_or_default();
    let delivered = app
        .try_state::<super::LiveHub>()
        .and_then(|hub| {
            hub.model
                .lock()
                .unwrap()
                .nav_files
                .get(base)
                .map(|native| native.listing(access, args, raw.clone()))
        })
        .unwrap_or(raw);
    Ok((values, delivered))
}

#[tauri::command]
pub async fn file_surface_prepare_view<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    frame_id: String,
    args: ListFiles,
) -> Result<String, String> {
    let (access, base) = file_surface::access(&app, &args, Some(&frame_id)).await?;
    let key = key(&base, &args, &access);
    {
        let hub = app.state::<super::LiveHub>();
        let model = hub.model.lock().unwrap();
        if model.files.views.contains_key(&key)
            && model.projections.has_view_subscribers("files", &key)
        {
            return Ok(key);
        }
    }
    let installed = is_installed_dir(&app, &base, &args);
    let root = if installed {
        PathBuf::from(args.dir.as_deref().unwrap())
    } else {
        access.logical_path(std::path::Path::new(args.dir.as_deref().unwrap_or("")))?
    };
    let (values, delivered) = list_native(&app, &access, &args, &base).await?;
    let rows = rows(values);
    let folder_rows = folder_rows(&access, &rows);
    let hub = app.state::<super::LiveHub>();
    let mut model = hub.model.lock().unwrap();
    model.projections.replace_view(
        "files",
        &key,
        delivered
            .into_iter()
            .map(|r| (Some(base.clone()), r))
            .collect(),
    )?;
    model.files.views.insert(
        key.clone(),
        FileView {
            access,
            args,
            team_key: base,
            root,
            rows,
            folder_rows,
        },
    );
    Ok(key)
}

/// Known content roots affected by a dropped-event scope, including inherited layers.
pub(crate) fn recovery_roots<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    scope: Option<&std::path::Path>,
    activation: bool,
) -> BTreeSet<(PathBuf, Option<PathBuf>)> {
    let Some(hub) = app.try_state::<super::LiveHub>() else {
        return BTreeSet::new();
    };
    let model = hub.model.lock().unwrap();
    let mut roots = BTreeSet::new();
    for view in model.files.views.values() {
        let own_affected = scope.is_none_or(|path| {
            crate::content_engine::dispatcher::paths_overlap(&view.access.own, path)
                || view.access.logical_path(path).is_ok()
                || view.access.view.as_ref().is_some_and(|inheritance| {
                    inheritance
                        .inherited
                        .iter()
                        .any(|layer| layer.content().overlaps(path))
                })
        });
        if own_affected {
            roots.insert((view.access.own.clone(), None));
        }
        if let Some(inheritance) = &view.access.view {
            for layer in &inheritance.inherited {
                let content = layer.content();
                if scope.is_none_or(|path| {
                    content.overlaps(path)
                        || (activation
                            && crate::content_engine::dispatcher::paths_overlap(
                                &view.access.own,
                                path,
                            ))
                }) {
                    roots.insert((
                        content.event_root().to_path_buf(),
                        Some(view.access.own.clone()),
                    ));
                }
            }
        }
    }
    roots
}

pub(super) fn start<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let handle = app.clone();
    app.listen_any("context-file-change", move |event| {
        let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else {
            return;
        };
        let Some(path) = payload["file_path"].as_str() else {
            return;
        };
        let hub = handle.state::<super::LiveHub>();
        let mut model = hub.model.lock().unwrap();
        model.files.pending.insert(path.into());
        if let Some(root) = payload["inherited_by"].as_str() {
            model.files.inherited.insert(root.into());
        }
        drop(model);
        hub.invalidate(super::FILES);
    });
    for event in ["agents:changed", "agents-changed"] {
        let handle = app.clone();
        app.listen_any(event, move |_| {
            let hub = handle.state::<super::LiveHub>();
            hub.model.lock().unwrap().files.agents_changed = true;
            hub.invalidate(super::FILES);
        });
    }
}

fn reset(view: &mut FileView) {
    view.rows = rows(json!({"rows":view.access.list_rows(view.root.to_str()).unwrap_or_default()}));
    view.folder_rows = folder_rows(&view.access, &view.rows);
}

/// Roots and child keys are logical. Physical paths only admit and identify files.
fn update(view: &mut FileView, paths: &BTreeMap<PathBuf, PathBuf>) {
    for (physical, logical) in paths {
        if view.root.starts_with(logical) {
            reset(view);
            return;
        }
        let Some(parent) = logical.parent().and_then(|p| p.to_str()) else {
            continue;
        };
        let key = logical.to_string_lossy().into_owned();
        let children = if std::path::Path::new(parent) == view.root {
            &mut view.rows
        } else if let Some(children) = view.folder_rows.get_mut(parent) {
            children
        } else {
            continue;
        };
        let Some(previous) = children
            .get(&key)
            .filter(|row| !file_surface::is_folder(row))
        else {
            reset(view);
            return;
        };
        let Ok(mut row) = view.access.row_for_path(physical) else {
            reset(view);
            return;
        };
        if row["id"] != previous["id"] {
            let Ok(winner) = view.access.row_for_logical_path(logical) else {
                reset(view);
                return;
            };
            // Only a farther, shadowed layer leaves the cached winner unchanged.
            if winner["id"] == previous["id"] {
                continue;
            }
            row = winner;
        }
        if file_surface::is_folder(&row) != file_surface::is_folder(previous) {
            reset(view);
            return;
        }
        view.access.tint_rows(std::slice::from_mut(&mut row));
        children.insert(key, row);
        if let Some(summary) = view.rows.get_mut(parent) {
            if let Some(children) = view.folder_rows.get(parent) {
                file_surface::folder_summary(summary, children.values());
            }
        }
    }
}

pub(super) async fn refresh(app: &tauri::AppHandle) -> Result<(), String> {
    refresh_with(app, |view| async move {
        file_surface::list_with_access(app.clone(), view.access, view.args, &view.team_key).await
    })
    .await
}

async fn refresh_with<R, F, Fut>(app: &tauri::AppHandle<R>, resolve: F) -> Result<(), String>
where
    R: tauri::Runtime,
    F: Fn(FileView) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let (paths, inherited, agents_changed, views) = {
        let hub = app.state::<super::LiveHub>();
        let mut model = hub.model.lock().unwrap();
        (
            std::mem::take(&mut model.files.pending),
            std::mem::take(&mut model.files.inherited),
            std::mem::take(&mut model.files.agents_changed),
            model
                .files
                .views
                .iter()
                .filter(|(key, _)| model.projections.has_view_subscribers("files", key))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    };
    let mut updated = Vec::new();
    for (key, mut view) in views {
        let changed: BTreeMap<_, _> = paths
            .iter()
            .filter_map(|physical| {
                let logical = view.access.logical_path(physical).ok()?;
                (logical.starts_with(&view.root) || view.root.starts_with(&logical))
                    .then(|| (physical.clone(), logical))
            })
            .collect();
        if !inherited.iter().any(|root| view.access.own == *root)
            && !(agents_changed && view.args.scope.as_deref() == Some("agent"))
            && changed.is_empty()
        {
            continue;
        }
        if matches!(view.args.scope.as_deref(), Some("story" | "agent")) {
            match resolve(view.clone()).await {
                Ok(value) => view.rows = rows(value),
                Err(error) if error == "File not found" => view.rows.clear(),
                Err(error) => {
                    log::warn!("files view {key}: {error}");
                    continue;
                }
            }
        } else {
            view = crate::commands::await_blocking("files.delta", move || {
                update(&mut view, &changed);
                Ok(view)
            })
            .await?;
        }
        updated.push((key, view));
    }
    let mut deltas = Vec::new();
    {
        let hub = app.state::<super::LiveHub>();
        let mut model = hub.model.lock().unwrap();
        for (key, view) in updated {
            let base = key.split('|').next().unwrap().to_owned();
            let delivered = view.rows.values().cloned().collect::<Vec<_>>();
            let delivered = model
                .nav_files
                .get(&base)
                .map(|native| native.listing(&view.access, &view.args, delivered.clone()))
                .unwrap_or(delivered);
            deltas.extend(
                model.projections.replace_view(
                    "files",
                    &key,
                    delivered
                        .into_iter()
                        .map(|r| (Some(base.clone()), r))
                        .collect(),
                )?,
            );
            model.files.views.insert(key, view);
        }
    }
    super::publish(app, deltas);
    Ok(())
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;

#[cfg(test)]
mod caller_scope_tests {
    use super::*;
    use crate::context_repo::caller_scope::tests::AppFixture;
    #[test]
    fn prepared_live_files_isolate_parent_and_child_in_both_warm_orders() {
        for child_first in [false, true] {
            let f = AppFixture::new();
            f.app.manage(f.fixture.service());
            f.app.manage(super::super::LiveHub::default());
            let child = f.frame(&f.fixture.path("parent", "panes/parent.html"), "workspace");
            let store = f
                .app
                .state::<std::sync::Arc<crate::dashboard_stream::frame::FrameStore>>();
            let mut grant = child.grant.clone();
            grant.team.as_mut().unwrap().team = "parent".into();
            let parent = store.mint(
                9321,
                grant,
                f.fixture.path("parent", "content").display().to_string(),
                child.document_path.clone(),
                child.asset_folder.clone(),
                child.policy.clone(),
                None,
            );
            tauri::async_runtime::block_on(async {
                let mut keys = BTreeMap::new();
                for (name, frame) in if child_first {
                    [("child", &child), ("parent", &parent)]
                } else {
                    [("parent", &parent), ("child", &child)]
                } {
                    let args = ListFiles {
                        team: Some("parent".into()),
                        ..Default::default()
                    };
                    let key = file_surface_prepare_view(
                        f.app.handle().clone(),
                        frame.frame_id.clone(),
                        args,
                    )
                    .await
                    .unwrap();
                    let hub = f.app.state::<super::super::LiveHub>();
                    let mut model = hub.model.lock().unwrap();
                    let snapshot = model
                        .projections
                        .subscribe_window(name, "files", &key, None, None)
                        .unwrap();
                    let text = serde_json::to_string(&snapshot).unwrap();
                    assert!(text.contains("shared.md"));
                    assert_eq!(text.contains("secret.md"), name == "parent");
                    keys.insert(name, key);
                }
                assert_ne!(keys["child"], keys["parent"]);
                let private = crate::context_repo::scratch_space::ensure_org_space(
                    &f.fixture.home,
                    &crate::domain::Principal::from(1u64),
                    "acme/ctx",
                )
                .unwrap()
                .0;
                std::fs::create_dir_all(private.content_dir()).unwrap();
                std::fs::write(private.content_dir().join("private-positive.md"), "private")
                    .unwrap();
                for (args, expected) in [
                    (ListFiles::default(), "secret.md"),
                    (
                        ListFiles {
                            team: Some("sibling".into()),
                            ..Default::default()
                        },
                        "secret.md",
                    ),
                    (
                        ListFiles {
                            scope: Some("private".into()),
                            ..Default::default()
                        },
                        "private-positive.md",
                    ),
                ] {
                    let key = file_surface_prepare_view(
                        f.app.handle().clone(),
                        child.frame_id.clone(),
                        args,
                    )
                    .await
                    .unwrap();
                    let hub = f.app.state::<super::super::LiveHub>();
                    let mut model = hub.model.lock().unwrap();
                    let snapshot = model
                        .projections
                        .subscribe_window("control", "files", &key, None, None)
                        .unwrap();
                    assert!(serde_json::to_string(&snapshot).unwrap().contains(expected));
                }
                let hidden = f.fixture.write("parent", "content/new-hidden.md", "hidden");
                f.app
                    .state::<super::super::LiveHub>()
                    .model
                    .lock()
                    .unwrap()
                    .files
                    .pending
                    .insert(hidden);
                refresh_with(f.app.handle(), |view| {
                    let app = f.app.handle().clone();
                    async move {
                        file_surface::list_with_access(app, view.access, view.args, &view.team_key)
                            .await
                    }
                })
                .await
                .unwrap();
                let hub = f.app.state::<super::super::LiveHub>();
                let mut model = hub.model.lock().unwrap();
                let child_snapshot = model.projections.snapshot("files", &keys["child"]).unwrap();
                assert!(!serde_json::to_string(&child_snapshot)
                    .unwrap()
                    .contains("new-hidden.md"));
                let parent_snapshot = model
                    .projections
                    .snapshot("files", &keys["parent"])
                    .unwrap();
                assert!(serde_json::to_string(&parent_snapshot)
                    .unwrap()
                    .contains("new-hidden.md"));

                assert!(model.files.views[&keys["child"]]
                    .access
                    .caller
                    .allows(&f.fixture.path("parent", "content/shared.md")));
                assert!(!model.files.views[&keys["child"]]
                    .access
                    .caller
                    .allows(&f.fixture.path("parent", "content/secret.md")));
            });
        }
    }
}
