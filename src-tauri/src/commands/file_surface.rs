// SPDX-License-Identifier: MIT
//! File navigation reads through the team sharing boundary.
use crate::context_repo::caller_scope::readback;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tauri::Manager;

pub const MAX_READ_BYTES: usize = 1536 * 1024;
const NOT_FOUND: &str = "File not found";

#[cfg(test)]
mod caller_scope_tests;

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListFiles {
    pub scope: Option<String>,
    pub team: Option<String>,
    pub story_id: Option<String>,
    pub agent_id: Option<String>,
    pub dir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContentAccess {
    pub own: PathBuf,
    pub(crate) view: Option<std::sync::Arc<crate::context_repo::inheritance::TeamView>>,
    pub(crate) caller: crate::context_repo::caller_scope::CallerScope,
}
impl ContentAccess {
    pub(crate) fn new(
        own: PathBuf,
        view: Option<std::sync::Arc<crate::context_repo::inheritance::TeamView>>,
        caller: crate::context_repo::caller_scope::CallerScope,
    ) -> Self {
        Self { own, view, caller }
    }
    pub fn admits_path(&self, path: &Path) -> Result<PathBuf, String> {
        if !self.caller.visible(path) {
            return Err(NOT_FOUND.into());
        }
        if let Some(view) = &self.view {
            use crate::context_repo::inheritance::Admit;
            return match view.admits_path(path) {
                Admit::Own | Admit::Inherited { .. } => view
                    .read_path(path)
                    .and_then(|path| path.canonicalize())
                    .map_err(|_| NOT_FOUND.into()),
                Admit::Denied if view.visible_directory(path) => {
                    path.canonicalize().map_err(|_| NOT_FOUND.into())
                }
                _ => Err(NOT_FOUND.into()),
            };
        }
        let root = self.own.canonicalize().map_err(|_| NOT_FOUND)?;
        let real = path.canonicalize().map_err(|_| NOT_FOUND)?;
        if real.starts_with(root) {
            Ok(real)
        } else {
            Err(NOT_FOUND.into())
        }
    }
    pub fn path(&self, path: Option<&str>) -> Result<PathBuf, String> {
        let path = path.map(PathBuf::from).unwrap_or_else(|| self.own.clone());
        self.admits_path(&if path.is_absolute() {
            path
        } else {
            self.own.join(path)
        })
    }
    pub(crate) fn row_for_path(&self, path: &Path) -> Result<Value, String> {
        let admitted = self.admits_path(path)?;
        let mut value = row(&admitted)?;
        value["root"] = json!(self.own);
        value["relativePath"] = json!(self.logical_path(path)?);
        if let Some(view) = &self.view {
            if let Some((_, team)) = crate::context_repo::inheritance::content_owner(path) {
                if view.inherited.iter().any(|layer| layer.team() == team) {
                    value["inheritedFrom"] = json!(team);
                }
            }
        }
        Ok(value)
    }
    pub(crate) fn tint_rows(&self, rows: &mut [Value]) {
        if let Ok(repo) = git2::Repository::discover(&self.own) {
            if let Some(workdir) = repo.workdir() {
                let mut options = git2::StatusOptions::new();
                options
                    .include_untracked(true)
                    .recurse_untracked_dirs(true)
                    .renames_head_to_index(true)
                    .renames_index_to_workdir(true);
                // Delta refreshes pass one row: restrict git work to that path.
                if rows.len() == 1 {
                    if let Some(relative) = rows[0]["path"]
                        .as_str()
                        .and_then(|path| Path::new(path).strip_prefix(workdir).ok())
                    {
                        options.pathspec(relative);
                    }
                }
                if let Ok(statuses) = repo.statuses(Some(&mut options)) {
                    for entry in statuses.iter() {
                        let delta = entry.index_to_workdir().or_else(|| entry.head_to_index());
                        let path = delta
                            .as_ref()
                            .and_then(|d| d.new_file().path())
                            .or_else(|| Result::ok(entry.path()).map(Path::new));
                        let Some(path) = path else {
                            continue;
                        };
                        let absolute = workdir.join(path);
                        let Some(row) = rows
                            .iter_mut()
                            .find(|row| row["path"].as_str() == absolute.to_str())
                        else {
                            continue;
                        };
                        let status = entry.status();
                        let state = if status.is_conflicted() {
                            "conflict"
                        } else if status
                            .intersects(git2::Status::WT_DELETED | git2::Status::INDEX_DELETED)
                        {
                            "deleted"
                        } else if status
                            .intersects(git2::Status::WT_RENAMED | git2::Status::INDEX_RENAMED)
                        {
                            "renamed"
                        } else if status.intersects(git2::Status::WT_NEW | git2::Status::INDEX_NEW)
                        {
                            "added"
                        } else {
                            "modified"
                        };
                        let old = delta.and_then(|d| d.old_file().path().map(Path::to_path_buf));
                        let old = old.filter(|old| {
                            let path = workdir.join(old);
                            self.caller.allows(&path)
                                && self.view.as_ref().map_or_else(
                                    || path.starts_with(&self.own),
                                    |view| {
                                        matches!(view.admits_path(&path),
                                        crate::context_repo::inheritance::Admit::Own
                                        | crate::context_repo::inheritance::Admit::Inherited { .. })
                                    },
                                )
                        });
                        let state = if state == "renamed" && old.is_none() {
                            "added"
                        } else {
                            state
                        };
                        row["gitState"] = json!(state);
                        if state == "renamed" {
                            if let Some(old) = old {
                                row["oldName"] =
                                    json!(old.file_name().unwrap_or_default().to_string_lossy());
                            }
                        }
                    }
                }
            }
        }
    }
    /// Coordinates shared by all layers; this maps identity, never authorizes a read.
    pub(crate) fn logical_path(&self, path: &Path) -> Result<PathBuf, String> {
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.own)
                .ok()
                .map(Path::to_path_buf)
                .or_else(|| {
                    self.view.as_ref()?.inherited.iter().find_map(|layer| {
                        layer
                            .content()
                            .relative_for_read(path)
                            .or_else(|| layer.content().relative_directory(path))
                    })
                })
                .ok_or(NOT_FOUND)?
        } else {
            path.to_path_buf()
        };
        if relative.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        }) {
            return Err(NOT_FOUND.into());
        }
        Ok(relative
            .components()
            .filter(|c| !matches!(c, std::path::Component::CurDir))
            .collect())
    }
    pub(crate) fn list_rows(&self, dir: Option<&str>) -> Result<Vec<Value>, String> {
        let relative = self.logical_path(Path::new(dir.unwrap_or("")))?;
        let mut rows = self.entries(&relative)?;
        for row in rows.values_mut().filter(|row| is_folder(row)) {
            let children =
                self.entries(Path::new(row["relativePath"].as_str().ok_or(NOT_FOUND)?))?;
            folder_summary(row, children.values());
        }
        let mut rows: Vec<Value> = rows.into_values().collect();
        self.tint_rows(&mut rows);
        Ok(rows)
    }
    fn layers(
        &self,
    ) -> Vec<(
        crate::context_repo::inheritance::ContentRoot,
        Option<String>,
    )> {
        let mut layers = Vec::new();
        if let Some(view) = &self.view {
            let own = view.own_content();
            if own.own_path() == Some(self.own.as_path()) {
                layers.push((own, None));
                layers.extend(
                    view.inherited
                        .iter()
                        .map(|layer| (layer.content(), Some(layer.team().to_owned()))),
                );
            } else {
                layers.extend(
                    view.inherited
                        .iter()
                        .filter(|layer| {
                            layer
                                .content()
                                .relative_directory(&self.own)
                                .is_some_and(|rel| rel.as_os_str().is_empty())
                        })
                        .map(|layer| (layer.content(), Some(layer.team().to_owned()))),
                );
            }
        }
        layers
    }
    /// Resolve one logical entry with the same nearest-layer precedence as listing.
    pub(crate) fn row_for_logical_path(&self, logical: &Path) -> Result<Value, String> {
        if self.view.is_none() {
            return self.row_for_path(&self.own.join(logical));
        }
        let logical = logical.to_str().ok_or(NOT_FOUND)?;
        self.layers()
            .into_iter()
            .filter_map(|(content, _)| content.file_path(logical))
            .find_map(|path| self.row_for_path(&path).ok())
            .ok_or_else(|| NOT_FOUND.into())
    }
    fn entries(
        &self,
        relative: &Path,
    ) -> Result<std::collections::BTreeMap<String, Value>, String> {
        use crate::agents::content_reader::NodeKind;
        let logical = relative.to_str().ok_or(NOT_FOUND)?;
        if self.view.is_none() {
            use codesense_fs_core::{FsService, RootSetFs};
            let path = self.path(Some(logical))?;
            return Ok(RootSetFs::new(vec![self.own.clone()])
                .list(&path)?
                .into_iter()
                .filter_map(|entry| self.row_for_path(Path::new(&entry.path)).ok())
                .map(|row| (row["name"].as_str().unwrap().to_owned(), row))
                .collect());
        }
        let mut rows = std::collections::BTreeMap::new();
        for (content, team) in self.layers() {
            for name in content.list(&self.caller, logical).unwrap_or_default() {
                let name = name.to_string_lossy();
                if codesense_fs_core::filters::is_ignored(&name)
                    || codesense_fs_core::filters::is_secret_path(&name)
                    || codesense_fs_core::filters::is_vcs_metadata_dir(&name)
                {
                    continue;
                }
                let child = relative.join(name.as_ref());
                let Some(child_str) = child.to_str() else {
                    continue;
                };
                let Ok(node) = content.node(&self.caller, child_str) else {
                    continue;
                };
                if node.kind == NodeKind::Dir {
                    let path = self.own.join(&child);
                    let mut value = json!({"id":path,"path":path,"name":name,"kind":"dir","relativePath":child,"root":self.own});
                    if let Some(team) = &team {
                        value["inheritedFrom"] = json!(team);
                    }
                    rows.entry(name.to_string()).or_insert(value);
                } else if node.kind == NodeKind::File {
                    let Some(path) = content.file_path(child_str) else {
                        continue;
                    };
                    if let Ok(mut value) = self.row_for_path(&path) {
                        value["relativePath"] = json!(child);
                        value["root"] = json!(self.own);
                        if let Some(team) = &team {
                            value["inheritedFrom"] = json!(team);
                        }
                        rows.entry(name.to_string()).or_insert(value);
                    }
                }
            }
        }
        Ok(rows)
    }
}

pub(crate) fn is_folder(row: &Value) -> bool {
    row["kind"] == "dir" || row["isDirectory"] == true
}

/// Derive snapshots and deltas from the same admitted, shadowed child set.
pub(crate) fn folder_summary<'a>(row: &mut Value, children: impl Iterator<Item = &'a Value>) {
    row["kind"] = json!("dir");
    for field in ["storyId", "title", "status", "epicFilePath", "isDirectory"] {
        row.as_object_mut().unwrap().remove(field);
    }
    let mut count = 0;
    for child in children {
        if child["kind"] == "story" {
            count += 1;
        }
        if child["kind"] == "epic" && !is_folder(child) {
            row["kind"] = json!("epic");
            row["isDirectory"] = json!(true);
            row["epicFilePath"] = child["path"].clone();
            for field in ["storyId", "title", "status"] {
                row[field] = child[field].clone();
            }
        }
    }
    row["storyCount"] = json!(count);
}

/// Resolve stored frame identity independently of the active team's UI state.
fn select_context(
    frame: Option<&crate::dashboard_stream::frame::FrameRecord>,
    args: &ListFiles,
    roots: &[crate::context_query::index::IndexRoot],
    ambient: Option<crate::mcp::state::ActiveTeam>,
) -> Result<crate::mcp::state::ActiveTeam, String> {
    use crate::context_repo::scratch_space::SCRATCH_TEAM;
    let Some(frame) = frame else {
        let mut selected = ambient.ok_or_else(|| NOT_FOUND.to_owned())?;
        if let Some(team) = &args.team {
            selected.team_name = team.clone();
        }
        return Ok(selected);
    };
    if let Some(team) = &frame.grant.team {
        if (frame.grant.scope == "team" || frame.grant.slot == "file-nav")
            && args
                .team
                .as_deref()
                .is_some_and(|requested| requested != team.team)
        {
            return Err(NOT_FOUND.into());
        }
        return Ok(crate::mcp::state::ActiveTeam {
            org_slug: team.org.clone(),
            team_name: args.team.clone().unwrap_or_else(|| team.team.clone()),
        });
    }
    let location =
        super::pane_bridge::frame_source_context(roots, &frame.document_path, &frame.content_root)
            .ok_or(NOT_FOUND)?;
    if location.layer != crate::context_query::model::Layer::Private
        || args.team.is_some()
        || args
            .scope
            .as_deref()
            .is_some_and(|scope| scope != "private")
    {
        return Err(NOT_FOUND.into());
    }
    Ok(crate::mcp::state::ActiveTeam {
        org_slug: location.org.ok_or(NOT_FOUND)?,
        team_name: SCRATCH_TEAM.into(),
    })
}

/// An absolute path returned by listing keeps its own synced team's context.
/// Only known content roots in the caller's org participate; frame restrictions
/// are then checked by the same selector used for listing.
fn path_selector(
    roots: &[crate::context_query::index::IndexRoot],
    org: &str,
    path: &str,
) -> ListFiles {
    let Some(location) =
        super::pane_bridge::doc_context(roots, path).filter(|l| l.org.as_deref() == Some(org))
    else {
        return ListFiles::default();
    };
    match location.layer {
        crate::context_query::model::Layer::Private => ListFiles {
            scope: Some("private".into()),
            ..Default::default()
        },
        crate::context_query::model::Layer::Team => ListFiles {
            team: location.team,
            ..Default::default()
        },
        _ => ListFiles::default(),
    }
}

pub(crate) async fn access<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    args: &ListFiles,
    frame_id: Option<&str>,
) -> Result<(ContentAccess, String), String> {
    let caller = crate::context_repo::caller_scope::for_page(app, None, frame_id)?;
    access_for(app, args, frame_id, caller).await
}

pub(crate) async fn access_for<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    args: &ListFiles,
    frame_id: Option<&str>,
    caller: crate::context_repo::caller_scope::CallerScope,
) -> Result<(ContentAccess, String), String> {
    let (home, uid) = super::route_to_context::resolve_ambient(app).await?;
    let frame = frame_id
        .map(|id| {
            app.state::<std::sync::Arc<crate::dashboard_stream::frame::FrameStore>>()
                .by_id(id)
                .ok_or("denied: frame identity")
        })
        .transpose()?;
    let active = select_context(
        frame.as_ref(),
        args,
        &crate::context_query::known_roots(app),
        app.state::<crate::mcp::state::ActiveTeamState>().get(),
    )?;
    let team = active.team_name.as_str();
    let principal = crate::domain::Principal::from(uid);
    let own = if args.scope.as_deref() == Some("private")
        || crate::context_repo::scratch_space::is_scratch_team(team)
    {
        crate::context_repo::scratch_space::org_space_at(&home, &principal, &active.org_slug)?
            .content_dir()
    } else {
        // Resolve only a synced team; this does not walk any ancestor content.
        let resolved =
            super::route_to_context::derive_context_for(app, &active.org_slug, team).await?;
        if !matches!(resolved, super::route_to_context::ContextResolution::Ok(_)) {
            return Err(NOT_FOUND.into());
        }
        let root = crate::context_repo::path_to_org_root(&home, &principal, &active.org_slug)?;
        let resolved = crate::context_repo::inheritance::team_view(&root, &active.org_slug, team);
        let own = resolved
            .own_content()
            .own_path()
            .ok_or(NOT_FOUND)?
            .to_path_buf();
        own
    };
    let view = if args.scope.as_deref() == Some("private")
        || crate::context_repo::scratch_space::is_scratch_team(team)
    {
        None
    } else {
        let root = crate::context_repo::path_to_org_root(&home, &principal, &active.org_slug)?;
        Some(caller.selected_view(
            crate::context_repo::inheritance::team_view(&root, &active.org_slug, team),
            team,
        )?)
    };
    Ok((
        ContentAccess::new(own, view, caller),
        crate::live_hub::view_key::team_key(&active.org_slug, team),
    ))
}

pub(crate) fn row(path: &Path) -> Result<Value, String> {
    let meta = std::fs::metadata(path).map_err(|_| NOT_FOUND)?;
    let mut kind = if meta.is_dir() { "dir" } else { "file" };
    let mut story_meta = json!({});
    if !meta.is_dir() {
        use std::io::Read;
        if let Some(format) = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(super::pipeline_format::detect_format)
        {
            let mut header = Vec::new();
            if std::fs::File::open(path)
                .and_then(|f| f.take(65536).read_to_end(&mut header))
                .is_ok()
            {
                let fields =
                    super::pipeline_format::extract_meta(format, &String::from_utf8_lossy(&header));
                story_meta = json!({"storyId":fields.story_id,"title":super::pipeline_format::extract_meta_title(format, &String::from_utf8_lossy(&header)).or_else(|| super::pipeline_format::extract_title(format, &String::from_utf8_lossy(&header))),"status":fields.status});
                if fields.document_type.as_deref() == Some("epic") {
                    kind = "epic";
                } else if fields.story_id.is_some() {
                    kind = "story";
                }
            }
        }
    }
    let mut value = json!({"id":path.to_string_lossy(),"path":path.to_string_lossy(),"name":path.file_name().unwrap_or_default().to_string_lossy(),"kind":kind,"size":(!meta.is_dir()).then_some(meta.len()),"mtime":(!meta.is_dir()).then(||meta.modified().ok()).flatten().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|t|t.as_millis() as u64)});
    value
        .as_object_mut()
        .unwrap()
        .extend(story_meta.as_object().unwrap().clone());
    Ok(value)
}

pub(crate) async fn list<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    args: ListFiles,
    frame_id: Option<String>,
) -> Result<Value, String> {
    let (access, team_key) = access(&app, &args, frame_id.as_deref()).await?;
    list_with_access(app, access, args, &team_key).await
}

pub(crate) async fn list_with_access<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    access: ContentAccess,
    args: ListFiles,
    team_key: &str,
) -> Result<Value, String> {
    if matches!(args.scope.as_deref(), Some("story" | "agent")) {
        let paths = scope_paths(&app, &access, &args, &team_key).await?;
        let dir = args
            .dir
            .as_deref()
            .map(|p| access.path(Some(p)))
            .transpose()?;
        let mut rows = paths
            .into_iter()
            .filter(|p| dir.as_ref().is_none_or(|d| p.starts_with(d)))
            .filter_map(|p| access.row_for_path(&p).ok())
            .collect::<Vec<_>>();
        access.tint_rows(&mut rows);
        return Ok(json!({"rows":rows}));
    }
    super::await_blocking("files.list", move || {
        Ok(json!({"rows":access.list_rows(args.dir.as_deref())?}))
    })
    .await
}

pub(crate) async fn call<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    operation: &str,
    args: Value,
    frame_id: Option<String>,
) -> Result<Value, String> {
    let caller = crate::context_repo::caller_scope::for_page(&app, None, frame_id.as_deref())?;
    call_for(app, operation, args, frame_id, caller).await
}

pub(crate) async fn call_for<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    operation: &str,
    args: Value,
    frame_id: Option<String>,
    caller: crate::context_repo::caller_scope::CallerScope,
) -> Result<Value, String> {
    if operation == "list" {
        let args = serde_json::from_value(args).map_err(|e| e.to_string())?;
        let (access, team_key) = access_for(&app, &args, frame_id.as_deref(), caller).await?;
        // Subscriptions and one-off reads share the native overlay, installed folders included.
        let (_, rows) =
            crate::live_hub::files::list_native(&app, &access, &args, &team_key).await?;
        return Ok(json!({"rows":rows}));
    }
    if operation == "delete" {
        let path = args["path"].as_str().ok_or("missing path")?.to_owned();
        let readable = caller.allows(Path::new(&path));
        let result = super::files::delete_content_file(app, path.clone()).await;
        return readback(readable, result.map(|()| json!({"deletedPath":path})));
    }
    if operation == "move" {
        let from = args["from"].as_str().ok_or("missing from")?.to_owned();
        let to = args["to"].as_str().ok_or("missing to")?.to_owned();
        require_delivery(&caller, Path::new(&from))?;
        let readable = caller.allows(Path::new(&to));
        let result = super::files::move_content_file(app, from.clone(), to.clone()).await;
        return readback(
            readable,
            result.map(|()| json!({"fromPath":from,"toPath":to})),
        );
    }
    let frame = frame_id
        .as_deref()
        .map(|id| {
            app.state::<std::sync::Arc<crate::dashboard_stream::frame::FrameStore>>()
                .by_id(id)
                .ok_or("denied: frame identity")
        })
        .transpose()?;
    let roots = crate::context_query::known_roots(&app);
    let context = select_context(
        frame.as_ref(),
        &ListFiles::default(),
        &roots,
        app.state::<crate::mcp::state::ActiveTeamState>().get(),
    )?;
    // Writes preserve baseline authorization; readback retains the resolved caller.
    let write = operation == "create";
    let read_scope = caller.clone();
    let authority = if write { Default::default() } else { caller };
    let requested = args["path"].as_str().or_else(|| args["from"].as_str());
    // Installed rows are browse-only native want-set files outside the selected root.
    // Exact native membership does not replace the trusted host's read authority.
    if matches!(operation, "resolve-open" | "read") {
        let installed = requested.and_then(|path| {
            crate::live_hub::stories::installed_file(
                &app,
                &crate::live_hub::view_key::team_key(&context.org_slug, &context.team_name),
                path,
            )
        });
        if let Some(path) = installed {
            read_scope.existing(&path)?;
            if operation == "resolve-open" {
                return Ok(json!({"path":path,"org":context.org_slug,"team":context.team_name}));
            }
            let cap = args["maxBytes"].as_u64().map(|n| n as usize);
            return crate::commands::await_blocking("file_surface.installed_read", move || {
                read_snapshot(&path, cap)
            })
            .await;
        }
    }
    let (current, _) = access_for(
        &app,
        &ListFiles::default(),
        frame_id.as_deref(),
        authority.clone(),
    )
    .await?;
    let inherited = requested.is_some_and(|path| {
        current.view.as_ref().is_some_and(|view| {
            crate::context_repo::inheritance::content_owner(Path::new(path))
                .is_some_and(|(_, owner)| view.inherited.iter().any(|layer| layer.team() == owner))
        })
    });
    let access = if inherited
        || requested.is_none_or(|path| {
            !Path::new(path).is_absolute() || Path::new(path).starts_with(&current.own)
        }) {
        current
    } else {
        let selector = requested
            .map(|path| path_selector(&roots, &context.org_slug, path))
            .unwrap_or_default();
        access_for(&app, &selector, frame_id.as_deref(), authority)
            .await?
            .0
    };
    let path = |key: &str| -> Result<&str, String> {
        args[key].as_str().ok_or_else(|| format!("missing {key}"))
    };
    match operation {
        "resolve-open" => {
            let path = access.path(Some(path("path")?))?;
            Ok(json!({"path":path,"org":context.org_slug,"team":context.team_name}))
        }
        "read" => {
            let path = access.path(Some(path("path")?))?;
            let cap = args["maxBytes"].as_u64().map(|n| n as usize);
            super::files::read_file_snapshot_for(
                app,
                path.to_string_lossy().into(),
                cap,
                access.caller,
            )
            .await
        }
        "create" => {
            let mut dest = PathBuf::from(destination(&access, path("path")?)?);
            let readable = read_scope.allows(&dest);
            match args["onCollision"].as_str().unwrap_or("fail") {
                "keep-both" => {
                    dest = keep_both(&dest);
                }
                "fail" => {}
                _ => return Err("onCollision must be keep-both or fail".into()),
            }
            let dest = dest.to_string_lossy().into_owned();
            let readable = readable && read_scope.allows(Path::new(&dest));
            let result = super::files::create_file(
                app,
                dest.clone(),
                args["content"].as_str().unwrap_or("").into(),
                None,
                None,
            )
            .await;
            readback(readable, result.map(|()| json!({"path":dest})))
        }
        _ => Err("unknown files operation".into()),
    }
}
/// Architecture: a move delivers source bytes, including every descendant of a folder.
fn require_delivery(
    caller: &crate::context_repo::caller_scope::CallerScope,
    path: &Path,
) -> Result<(), String> {
    if path.is_dir() && caller.visible(path) {
        for entry in walkdir::WalkDir::new(path).min_depth(1).follow_links(true) {
            let entry = entry.map_err(|_| NOT_FOUND.to_owned())?;
            if !caller.visible(entry.path()) {
                return Err(NOT_FOUND.into());
            }
        }
        return Ok(());
    }
    if matches!(caller, crate::context_repo::caller_scope::CallerScope::Team(view)
        if matches!(view.admits_path(path), crate::context_repo::inheritance::Admit::Inherited { .. }))
    {
        caller.existing(path)
    } else {
        caller.require(path)
    }
}

fn destination(access: &ContentAccess, path: &str) -> Result<String, String> {
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        access.own.join(path)
    };
    access.caller.require(&path)?;
    let mut parent = path.parent().ok_or(NOT_FOUND)?;
    while !parent.exists() {
        parent = parent.parent().ok_or(NOT_FOUND)?;
    }
    access.admits_path(parent)?;
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(NOT_FOUND.into());
    }
    if path.exists() {
        access.admits_path(&path)?;
    }
    Ok(path.to_string_lossy().into())
}
fn keep_both(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.into();
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 2.. {
        let candidate = path.with_file_name(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}
#[tauri::command]
pub async fn file_surface_call(
    app: tauri::AppHandle,
    operation: String,
    args: Value,
    frame_id: Option<String>,
) -> Result<Value, String> {
    call(app, &operation, args, frame_id).await
}

fn viewer_url(path: &Path) -> String {
    use base64::Engine;
    let root = path.parent().unwrap_or(Path::new("/"));
    let root =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(root.to_string_lossy().as_bytes());
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("/viewer/{root}/{encoded}")
}
async fn scope_paths<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    access: &ContentAccess,
    args: &ListFiles,
    team_key: &str,
) -> Result<Vec<PathBuf>, String> {
    let (home, uid) = super::route_to_context::resolve_ambient(app).await?;
    let principal = crate::domain::Principal::from(uid);
    let access = access.clone();
    let args = args.clone();
    let (org, team) = team_key
        .strip_prefix("team:")
        .and_then(|k| k.rsplit_once('/'))
        .ok_or(NOT_FOUND)?;
    let (org, team) = (org.to_owned(), team.to_owned());
    let cache = app
        .state::<crate::install::frontmatter_cache::SharedFrontmatterCache>()
        .inner()
        .clone();
    super::await_blocking("files.scope", move || {
        if args.scope.as_deref() == Some("story") {
            let id = args.story_id.as_deref().ok_or(NOT_FOUND)?;
            let snapshot = crate::install::frontmatter_cache::FrontmatterCache::snapshot(&cache);
            let entries: Vec<_> = snapshot
                .by_story_id(id)
                .into_iter()
                .filter(|e| access.admits_path(&e.path).is_ok())
                .collect();
            let [entry] = entries.as_slice() else {
                return Err(NOT_FOUND.into());
            };
            let ctx = super::route_to_context::ActiveContext::for_team(home, uid, org, team)
                .to_minimal_resolver_context();
            let epic = super::route_to_context::resolve_story_epic_dir(
                &snapshot,
                &ctx,
                entry.source_repo(),
                id,
                "files.list",
            )
            .map_err(|_| NOT_FOUND)?;
            access.admits_path(&epic)?;
            let slug = crate::domain::SourceRepoSlug::parse_owner_repo(entry.source_repo())
                .ok_or(NOT_FOUND)?;
            return super::route_to_context::remote_story_files(&ctx, &slug, id, &cache, None)
                .map(|set| set.files)
                .map_err(|_| NOT_FOUND.into());
        }
        let id = args.agent_id.as_deref().ok_or(NOT_FOUND)?;
        let key = crate::agents::AgentKey::parse(id).ok_or(NOT_FOUND)?;
        if !access.caller.definition(&home, &principal, &key) {
            return Err(NOT_FOUND.into());
        }
        let agent =
            crate::agents::service::read_current(&home, &principal, &key).map_err(|_| NOT_FOUND)?;
        let target = crate::agents::manifest_resolve::ManifestTarget::for_launch(&key, None);
        let resolved = crate::agents::manifest_resolve::context::resolve_context_with(
            &access,
            &home,
            &principal,
            crate::agents::manifest_resolve::ContextRequest {
                agent: &agent,
                target: &target,
                repo_override: None,
                team_workspace: true,
            },
        );
        Ok(resolved.files.into_iter().map(|f| f.source).collect())
    })
    .await
}
impl crate::agents::content_reader::ContentReader for ContentAccess {
    fn node(&self, path: &Path) -> std::io::Result<crate::agents::content_reader::Node> {
        self.admits_path(path)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        crate::agents::content_reader::ContentReader::node(
            &crate::agents::content_reader::FsContentReader,
            path,
        )
    }
    fn list(&self, path: &Path) -> std::io::Result<Vec<std::ffi::OsString>> {
        self.admits_path(path)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        let entries = crate::agents::content_reader::ContentReader::list(
            &crate::agents::content_reader::FsContentReader,
            path,
        )?;
        Ok(entries
            .into_iter()
            .filter(|name| self.admits_path(&path.join(name)).is_ok())
            .collect())
    }
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn std::io::Read>> {
        self.admits_path(path)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        crate::agents::content_reader::ContentReader::open(
            &crate::agents::content_reader::FsContentReader,
            path,
        )
    }
    fn file_len(&self, path: &Path) -> std::io::Result<u64> {
        self.admits_path(path)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        crate::agents::content_reader::ContentReader::file_len(
            &crate::agents::content_reader::FsContentReader,
            path,
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) struct SurfaceFixture {
        pub(crate) app: tauri::App<tauri::test::MockRuntime>,
        pub(crate) org_root: PathBuf,
        pub(crate) team_root: PathBuf,
        private_root: PathBuf,
        _home: crate::routines::test_support::TempHome,
    }
    impl SurfaceFixture {
        pub(crate) fn new() -> Self {
            use crate::routines::test_support::{launch_app, TempHome, ORG, TEAM};
            let home = TempHome::new();
            // Commands return canonical paths, so the fixture home uses that spelling too.
            home.act_as(&home.root().canonicalize().unwrap());
            let codesense_home = home.codesense_home();
            let org_root = crate::context_repo::path_to_org_root(
                &codesense_home,
                &crate::domain::Principal::from(1u64),
                ORG,
            )
            .unwrap();
            let team_root = crate::agents::resolve::tests::content(
                &codesense_home,
                &crate::domain::Principal::from(1u64),
                TEAM,
            );
            let private_root = crate::context_repo::scratch_space::org_space_at(
                &codesense_home,
                &crate::domain::Principal::from(1u64),
                ORG,
            )
            .unwrap()
            .content_dir();
            std::fs::create_dir_all(&private_root).unwrap();
            let app = launch_app(&codesense_home);
            app.manage(std::sync::Arc::new(
                crate::dashboard_stream::frame::FrameStore::default(),
            ));
            app.state::<crate::mcp::state::ActiveTeamState>().update(
                crate::mcp::state::ActiveTeam {
                    org_slug: ORG.into(),
                    team_name: TEAM.into(),
                },
            );
            Self {
                _home: home,
                app,
                org_root,
                team_root,
                private_root,
            }
        }
        pub(crate) fn frame(
            &self,
            slot: &str,
            team: Option<&str>,
            scope: &str,
        ) -> crate::dashboard_stream::frame::FrameRecord {
            use crate::dashboard_stream::frame::*;
            use crate::routines::test_support::ORG;
            let document = if team.is_some() {
                self.team_root.join("dashboard.html")
            } else if slot == "centre" {
                self.private_root.join("dashboard.html")
            } else {
                self.private_root.parent().unwrap().join("panes/tree.html")
            };
            std::fs::create_dir_all(document.parent().unwrap()).unwrap();
            std::fs::write(&document, "<!doctype html><title>Fixture</title>").unwrap();
            let content_root = if slot == "centre" && team.is_none() {
                &self.private_root
            } else {
                &self.team_root
            };
            self.app.state::<std::sync::Arc<FrameStore>>().mint(
                9321,
                FrameGrant {
                    frame_id: String::new(),
                    principal: "1".into(),
                    dashboard: document.to_string_lossy().into(),
                    slot: slot.into(),
                    team: team.map(|team| FrameTeam {
                        org: ORG.into(),
                        team: team.into(),
                    }),
                    scope: scope.into(),
                },
                content_root.to_string_lossy().into(),
                document.to_string_lossy().into(),
                document.parent().unwrap().to_string_lossy().into(),
                FramePolicy::default(),
                None,
            )
        }
    }

    #[test]
    fn listed_private_paths_resolve_in_the_requesting_viewer_team() {
        use crate::routines::test_support::{ORG, TEAM};
        let fixture = SurfaceFixture::new();
        let path = fixture.private_root.join("private.md");
        std::fs::write(&path, "private file").unwrap();
        let frame = fixture.frame("file-nav", Some(TEAM), "open");
        let target = tauri::async_runtime::block_on(call(
            fixture.app.handle().clone(),
            "resolve-open",
            json!({"path":path}),
            Some(frame.frame_id),
        ))
        .unwrap();
        assert_eq!(target, json!({"path":path,"org":ORG,"team":TEAM}));
    }

    #[test]
    fn stored_private_frames_list_read_and_create_through_production_access() {
        let fixture = SurfaceFixture::new();
        std::fs::write(fixture.private_root.join("seed.md"), "private seed").unwrap();
        tauri::async_runtime::block_on(async {
            for slot in ["centre", "right-pane"] {
                let frame = fixture.frame(slot, None, "private");
                let app = fixture.app.handle();
                assert!(crate::context_query::known_roots(app)
                    .iter()
                    .any(|root| root.dir == fixture.private_root));
                // Read/create have no explicit scope. They must adopt the source's scratch root.
                let (selected, _) = access(app, &ListFiles::default(), Some(&frame.frame_id))
                    .await
                    .unwrap();
                assert_eq!(selected.own, fixture.private_root);
                let listed = list(
                    app.clone(),
                    ListFiles {
                        scope: Some("private".into()),
                        ..Default::default()
                    },
                    Some(frame.frame_id.clone()),
                )
                .await
                .unwrap();
                assert!(listed["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["name"] == "seed.md"));
                let read = call(
                    app.clone(),
                    "read",
                    json!({"path":"seed.md"}),
                    Some(frame.frame_id.clone()),
                )
                .await
                .unwrap();
                assert_eq!(read["text"], "private seed");
                let name = format!("created-{slot}.md");
                let created = call(
                    app.clone(),
                    "create",
                    json!({"path":name,"content":"created through command","onCollision":"fail"}),
                    Some(frame.frame_id.clone()),
                )
                .await
                .unwrap();
                assert_eq!(
                    created["path"],
                    fixture.private_root.join(&name).to_string_lossy().as_ref()
                );
                let read_created = call(
                    app.clone(),
                    "read",
                    json!({"path":name}),
                    Some(frame.frame_id.clone()),
                )
                .await
                .unwrap();
                assert_eq!(read_created["text"], "created through command");
                let mut limited = frame.clone();
                limited.effective_grant = vec!["files:read".into()];
                let mut dispatcher = crate::verbs::Dispatcher::default();
                assert!(matches!(
                    dispatcher.dispatch_frame(
                        &limited,
                        "files.read",
                        json!({}),
                        &crate::verbs::Policy::default(),
                        0
                    ),
                    crate::verbs::Decision::Admit { .. }
                ));
                assert!(matches!(
                    dispatcher.dispatch_frame(
                        &limited,
                        "files.create",
                        json!({}),
                        &crate::verbs::Policy::default(),
                        0
                    ),
                    crate::verbs::Decision::Deny { .. }
                ));
                limited.effective_grant.clear();
                assert!(matches!(
                    dispatcher.dispatch_frame(
                        &limited,
                        "files.read",
                        json!({}),
                        &crate::verbs::Policy::default(),
                        0
                    ),
                    crate::verbs::Decision::Deny { .. }
                ));
            }
        });
    }

    #[test]
    fn listed_other_team_paths_read_through_production_call_for_stored_open_frames_and_mcp() {
        use crate::routines::test_support::TEAM;
        let fixture = SurfaceFixture::new();
        crate::agents::resolve::tests::write(
            &fixture.org_root,
            "teams.yml",
            format!("teams:\n  - name: {TEAM}\n  - name: other\n").as_bytes(),
        );
        let other = crate::agents::resolve::tests::content(
            &fixture._home.codesense_home(),
            &crate::domain::Principal::from(1u64),
            "other",
        );
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("other.md"), "other team").unwrap();
        let open = fixture.frame("right-pane", Some(TEAM), "open");
        let safe = fixture.frame("right-pane", Some(TEAM), "team");
        tauri::async_runtime::block_on(async {
            for frame_id in [Some(open.frame_id.clone()), None] {
                let app = fixture.app.handle();
                let listed = call(
                    app.clone(),
                    "list",
                    json!({"scope":"team","team":"other"}),
                    frame_id.clone(),
                )
                .await
                .unwrap();
                let row = listed["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["name"] == "other.md")
                    .unwrap();
                let read = call(
                    app.clone(),
                    "read",
                    json!({"path":row["path"]}),
                    frame_id.clone(),
                )
                .await
                .unwrap();
                assert_eq!(read["text"], "other team");
                let destination = other.join(if frame_id.is_some() {
                    "from-frame.md"
                } else {
                    "from-mcp.md"
                });
                let created = call(
                    app.clone(),
                    "create",
                    json!({"path":destination,"content":"selected team","onCollision":"fail"}),
                    frame_id,
                )
                .await
                .unwrap();
                assert_eq!(created["path"], destination.to_string_lossy().as_ref());
            }
            let denied = call(
                fixture.app.handle().clone(),
                "read",
                json!({"path":other.join("other.md")}),
                Some(safe.frame_id),
            )
            .await
            .unwrap_err();
            assert_eq!(denied, NOT_FOUND);
        });
    }
    #[test]
    fn own_content_reader_refuses_parent_other_team_private_and_missing_identically() {
        let home = tempfile::tempdir().unwrap();
        let own = home.path().join("child/content");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("own.md"), "own").unwrap();
        let access = ContentAccess::new(
            own.clone(),
            None,
            crate::context_repo::caller_scope::CallerScope::Human,
        );
        assert!(access.path(Some("own.md")).is_ok());
        for folder in ["parent", "other", "private"] {
            let path = home.path().join(folder).join("secret.md");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "secret").unwrap();
            assert_eq!(
                access.admits_path(&path).unwrap_err(),
                access.path(Some("missing.md")).unwrap_err()
            );
            assert!(crate::agents::content_reader::ContentReader::open(&access, &path).is_err());
        }
    }
    #[test]
    fn read_caps_at_a_utf8_boundary_and_returns_a_real_viewer_url_with_full_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a space.md");
        std::fs::write(&path, "aéz").unwrap();
        let result = read_snapshot(&path, Some(2)).unwrap();
        assert_eq!(result["text"], "a");
        assert_eq!(result["size"], 4);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["sha256"].as_str().unwrap().len(), 64);
        assert!(result["bytesUrl"].as_str().unwrap().starts_with("/viewer/"));
        assert!(result["bytesUrl"]
            .as_str()
            .unwrap()
            .ends_with("a%20space.md"));
        let binary = dir.path().join("b");
        std::fs::write(&binary, [255, 0]).unwrap();
        assert!(read_snapshot(&binary, None).unwrap().get("text").is_none());
    }
    #[test]
    fn streaming_read_preserves_full_hash_and_size_with_a_capped_utf8_prefix() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large file.md");
        let text = format!("aé{}", "z".repeat(MAX_READ_BYTES + 100));
        std::fs::write(&path, &text).unwrap();
        let value = read_snapshot(&path, Some(2)).unwrap();
        assert_eq!(value["text"], "a");
        assert_eq!(value["size"], text.len());
        assert_eq!(value["truncated"], true);
        assert_eq!(
            value["sha256"],
            format!("{:x}", Sha256::digest(text.as_bytes()))
        );
        assert!(value["bytesUrl"]
            .as_str()
            .unwrap()
            .ends_with("large%20file.md"));
        let capped = read_snapshot(&path, None).unwrap();
        assert_eq!(capped["text"].as_str().unwrap().len(), MAX_READ_BYTES);
        std::fs::write(&path, [0, 255, 1]).unwrap();
        let binary = read_snapshot(&path, None).unwrap();
        assert!(binary.get("text").is_none());
        assert!(binary["bytesUrl"].as_str().unwrap().starts_with("/viewer/"));
    }
    #[test]
    fn create_keep_both_uses_the_first_available_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.md");
        std::fs::write(&file, "original").unwrap();
        std::fs::write(dir.path().join("file (2).md"), "second").unwrap();
        assert_eq!(keep_both(&file), dir.path().join("file (3).md"));
        assert_eq!(std::fs::read_to_string(file).unwrap(), "original");
    }
}

pub(crate) fn read_snapshot(path: &Path, max_bytes: Option<usize>) -> Result<Value, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let cap = max_bytes.unwrap_or(MAX_READ_BYTES).min(MAX_READ_BYTES);
    let mut file = std::fs::File::open(path).map_err(|_| NOT_FOUND)?;
    let mut hash = Sha256::new();
    let mut prefix = Vec::new();
    let mut size = 0usize;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|_| NOT_FOUND)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        size += n;
        let retain = n.min((cap + 4).saturating_sub(prefix.len()));
        prefix.extend_from_slice(&buffer[..retain]);
    }
    let mut value =
        json!({"size":size,"truncated":size > cap,"sha256":format!("{:x}",hash.finalize())});
    let text = match std::str::from_utf8(&prefix) {
        Ok(text) => Some(text),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&prefix[..error.valid_up_to()]).ok()
        }
        Err(_) => None,
    }
    .filter(|text| !text.contains('\0'));
    if let Some(text) = text {
        let mut end = cap.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        value["text"] = text[..end].into();
    }
    if size > cap || text.is_none() {
        value["bytesUrl"] = viewer_url(path).into();
    }
    Ok(value)
}

#[cfg(test)]
#[path = "file_surface_tests.rs"]
mod inheritance_tests;
