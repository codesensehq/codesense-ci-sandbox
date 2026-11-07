// SPDX-License-Identifier: MIT
//! Native navigation derivations enter one delivery cache; the existing scan owns ingress.
use crate::commands::file_surface::{ContentAccess, ListFiles};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};
use tauri::Manager;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeFiles {
    tree: Value,
    starter_page: Option<StarterPage>,
    merge_paths: Vec<String>,
    repos: BTreeMap<String, Value>,
    #[serde(skip)]
    index: OnceLock<FileIndex>,
}
#[derive(Clone, Default)]
struct FileIndex {
    by_logical: BTreeMap<String, Value>,
    children: BTreeMap<String, Vec<String>>,
}
impl PartialEq for NativeFiles {
    fn eq(&self, other: &Self) -> bool {
        self.tree == other.tree
            && self.starter_page == other.starter_page
            && self.merge_paths == other.merge_paths
            && self.repos == other.repos
    }
}
#[derive(Clone, Deserialize, PartialEq)]
struct StarterPage {
    path: String,
    root: String,
}

fn relative(path: &str, root: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        path.strip_prefix(root).ok().map(Path::to_path_buf)
    } else {
        Some(path.to_path_buf())
    }
}
impl NativeFiles {
    fn templates(&self) -> &FileIndex {
        self.index.get_or_init(|| {
        fn walk(node: &Value, parent: &str, parent_logical: &str, rows: &mut Vec<Value>) {
            let path = node["absoluteDirPath"].as_str().unwrap_or_default();
            let dir = node["dirPath"].as_str().unwrap_or_default();
            let logical = if path.starts_with("installed://") {
                path
            } else {
                dir
            };
            if !parent.is_empty() {
                let slug = path.strip_prefix("installed://").and_then(|p| {
                    let mut parts = p.split('/');
                    Some(format!("{}/{}", parts.next()?, parts.next()?))
                });
                rows.push(json!({"id":path,"path":path,"name":node["dirName"],"kind":"dir","isDirectory":true,
                    "relativePath":logical,"parentPath":parent,"parentLogical":parent_logical,"installedSourceRepo":slug}));
            }
            for doc in node["docs"].as_array().into_iter().flatten() {
                let name = doc["fileName"].as_str().unwrap_or_default();
                let relative = if logical.is_empty() {
                    name.to_owned()
                } else {
                    format!("{logical}/{name}")
                };
                rows.push(json!({"id":doc["filePath"],"path":doc["filePath"],"name":name,"kind":"file","isDirectory":false,
                    "relativePath":relative,"parentPath":path,"parentLogical":logical,"shadows":doc["shadowsTeamFile"].as_bool().unwrap_or(false),
                    "overrides":doc["overridesTeam"],"installedSourceRepo":doc["installedSourceRepo"],
                    "inheritedFrom":doc["sourceTeam"],"metadataValid":doc["metadataValid"].as_bool().unwrap_or(true),
                    "metadataInvalidReason":doc["metadataInvalidReason"]}));
            }
            for sub in node["subEpics"].as_array().into_iter().flatten() {
                walk(sub, path, logical, rows);
            }
        }
        let mut rows = Vec::new();
        walk(&self.tree, "", "", &mut rows);
        let mut index = FileIndex::default();
        for row in rows {
            let logical = row["relativePath"].as_str().unwrap().to_owned();
            index.children.entry(row["parentLogical"].as_str().unwrap().to_owned()).or_default().push(logical.clone());
            index.by_logical.insert(logical, row);
        }
        index
        })
    }
    pub(super) fn listing(
        &self,
        access: &ContentAccess,
        args: &ListFiles,
        raw: Vec<Value>,
    ) -> Vec<Value> {
        if args.scope.as_deref() == Some("private") {
            return raw;
        }
        let dir = args.dir.as_deref().unwrap_or("");
        let target = if dir.starts_with("installed://") {
            dir.to_owned()
        } else {
            access
                .logical_path(Path::new(dir))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let templates = self.templates();
        let by_logical = &templates.by_logical;
        let mut rows: BTreeMap<String, Value> = raw
            .into_iter()
            .map(|mut row| {
                let logical = row["relativePath"].as_str().unwrap_or_default().to_owned();
                if let Some(native) = by_logical.get(logical.as_str()) {
                    if native["installedSourceRepo"].is_null() {
                        for field in [
                            "shadows",
                            "overrides",
                            "installedSourceRepo",
                            "metadataValid",
                            "metadataInvalidReason",
                        ] {
                            if let Some(value) = native.get(field) {
                                row[field] = value.clone();
                            }
                        }
                    }
                }
                (logical, row)
            })
            .collect();
        if matches!(args.scope.as_deref(), None | Some("team")) {
            for logical in templates.children.get(&target).into_iter().flatten() {
                let native = &templates.by_logical[logical];
                if !native["installedSourceRepo"].is_null()
                    && (native["isDirectory"] == true
                        || native["path"].as_str().is_some_and(|path| {
                            access.caller.allows(Path::new(path))
                        }))
                {
                    rows.insert(
                        native["relativePath"].as_str().unwrap().to_owned(),
                        native.clone(),
                    );
                }
            }
        }
        for row in rows.values_mut() {
            row["root"] = json!(access.own);
            row["starterPage"] = json!(self.starter_page.as_ref().is_some_and(|starter| {
                relative(row["path"].as_str().unwrap_or_default(), &starter.root)
                    .zip(relative(&starter.path, &starter.root))
                    .is_some_and(|(a, b)| a == b)
            }));
            row["mergeMarked"] = json!(row["relativePath"]
                .as_str()
                .is_some_and(|p| self.merge_paths.iter().any(|m| m == p)));
            if row.get("shadows").is_none() {
                row["shadows"] = json!(false);
            }
            if row.get("overrides").is_none() {
                row["overrides"] = Value::Null;
            }
            let repo = row["installedSourceRepo"]
                .as_str()
                .and_then(|slug| self.repos.get(slug));
            let glyph = repo.map(|r| r["glyph"].clone()).unwrap_or(Value::Null);
            let tint = repo.map(|r| r["tint"].clone()).unwrap_or(Value::Null);
            row["repoGlyph"] = glyph;
            row["repoTint"] = tint;
        }
        rows.into_values().collect()
    }
    pub(super) fn installed_dir(&self, dir: &str) -> bool {
        dir.starts_with("installed://")
            && self
                .templates()
                .by_logical
                .get(dir)
                .is_some_and(|row| row["isDirectory"] == true)
    }
    fn installed_file(&self, path: &str) -> Option<PathBuf> {
        self.templates()
            .by_logical
            .values()
            .find(|row| {
                row["path"] == path
                    && row["installedSourceRepo"].is_string()
                    && row["isDirectory"] == false
            })
            .map(|_| PathBuf::from(path))
    }
}

pub(crate) fn installed_file<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    team_key: &str,
    path: &str,
) -> Option<PathBuf> {
    let hub = app.try_state::<super::LiveHub>()?;
    let model = hub.model.lock().unwrap();
    model.nav_files.get(team_key)?.installed_file(path)
}

pub(super) fn publish_native<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    team_key: &str,
    rows: Vec<Value>,
    files: NativeFiles,
) -> Result<(), String> {
    let (org, team) = team_key
        .rsplit_once(':')
        .ok_or("navigation requires a team")?;
    let key = super::view_key::team_key(org, team);
    // Directory subscriptions share one index of the native snapshot, built outside the hub lock.
    files.templates();
    let hub = app.state::<super::LiveHub>();
    let deltas = {
        let mut model = hub.model.lock().unwrap();
        let mut deltas = model.projections.replace_view(
            "stories",
            &key,
            rows.into_iter().map(|r| (Some(key.clone()), r)).collect(),
        )?;
        if model.nav_files.get(&key) != Some(&files) {
            let updates: Vec<_> = model
                .files
                .views
                .iter()
                .filter(|(k, v)| {
                    v.team_key == key && model.projections.has_view_subscribers("files", k)
                })
                .map(|(k, v)| {
                    (
                        k.clone(),
                        files.listing(&v.access, &v.args, v.rows.values().cloned().collect()),
                    )
                })
                .collect();
            for (view, rows) in updates {
                deltas.extend(model.projections.replace_view(
                    "files",
                    &view,
                    rows.into_iter().map(|r| (Some(key.clone()), r)).collect(),
                )?);
            }
            model.nav_files.insert(key, files);
        }
        deltas
    };
    super::publish(app, deltas);
    Ok(())
}

#[tauri::command]
pub async fn nav_model_publish(
    app: tauri::AppHandle,
    team_key: String,
    rows: Vec<Value>,
    files: NativeFiles,
) -> Result<(), String> {
    publish_native(&app, &team_key, rows, files)
}

#[cfg(test)]
mod tests;
