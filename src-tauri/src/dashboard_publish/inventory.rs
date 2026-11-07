// SPDX-License-Identifier: MIT
use super::{
    client::{CallError, Client, DashboardView, DASH_HOST},
    keys, manifest,
    state::{self, LocalState},
};
use crate::commands::credential_store::CredentialStore;
use crate::context_query::index::IndexRoot;
use crate::context_query::model::Layer;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tauri::Manager;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryRow {
    pub slug: String,
    pub title: String,
    pub folder: Option<String>,
    pub location: &'static str,
    pub local_state: Option<LocalState>,
    pub hosted: Option<DashboardView>,
    pub pushed_at: Option<String>,
    pub checked_at: Option<String>,
    pub error: Option<String>,
    pub different_key: bool,
    pub team: Option<InventoryTeam>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryTeam {
    pub org: String,
    pub team: String,
    pub shared_from: Option<String>,
    pub relative_folder: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub host: Option<String>,
    pub source: &'static str,
    pub checked_at: String,
    pub error: Option<String>,
    pub rows: Vec<InventoryRow>,
    pub teams: Vec<InventoryTeamHeader>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryTeamHeader {
    pub org: String,
    pub team: String,
}

pub async fn inventory_with(
    data: &Path,
    store: Arc<dyn CredentialStore>,
    host: Option<&str>,
) -> Result<Inventory, String> {
    inventory_with_teams(data, store, host, &[], false).await
}

async fn inventory_with_teams(
    data: &Path,
    store: Arc<dyn CredentialStore>,
    host: Option<&str>,
    roots: &[IndexRoot],
    local_only: bool,
) -> Result<Inventory, String> {
    let mut rows = BTreeMap::new();
    for slug in state::list(data)? {
        let record = state::load(data, &slug)?;
        let folder = record.folder.clone().filter(|f| Path::new(f).is_dir());
        let title = folder
            .as_ref()
            .and_then(|f| manifest::load(Path::new(f)).ok())
            .map(|m| m.title)
            .unwrap_or_else(|| slug.clone());
        rows.insert(
            slug.clone(),
            InventoryRow {
                slug,
                title,
                folder,
                location: "local",
                local_state: Some(record.state),
                hosted: None,
                pushed_at: record.pushed_at,
                checked_at: record.checked_at,
                error: record
                    .last_error
                    .and_then(|e| serde_json::to_value(e).ok())
                    .and_then(|v| v.as_str().map(str::to_owned)),
                different_key: false,
                team: None,
            },
        );
    }
    let mut result = Inventory {
        host: host.map(str::to_owned),
        source: "local",
        checked_at: chrono::Utc::now().to_rfc3339(),
        error: None,
        rows: vec![],
        teams: team_headers(roots),
    };
    if !local_only {
        if let Some(token) = keys::token(store).await? {
            if let Some(client) = host.map(Client::new).transpose()? {
                match client.list(&token).await {
                    Ok(hosted) => {
                        result.source = "hosted";
                        for row in rows.values_mut() {
                            row.different_key =
                                matches!(row.local_state, Some(LocalState::Published))
                                    && !hosted.iter().any(|h| h.slug == row.slug);
                        }
                        for dashboard in hosted {
                            let row = rows.entry(dashboard.slug.clone()).or_insert_with(|| {
                                InventoryRow {
                                    slug: dashboard.slug.clone(),
                                    title: dashboard.title.clone(),
                                    folder: None,
                                    location: "hosted",
                                    local_state: None,
                                    hosted: None,
                                    pushed_at: None,
                                    checked_at: None,
                                    error: None,
                                    different_key: false,
                                    team: None,
                                }
                            });
                            row.location = if row.folder.is_some() {
                                "both"
                            } else {
                                "hosted"
                            };
                            row.title = dashboard.title.clone();
                            row.hosted = Some(dashboard);
                        }
                    }
                    Err(CallError::Status { status: 404, .. }) => {
                        result.source = "unsupported";
                    }
                    Err(CallError::Status { status: 401, .. }) => {
                        result.source = "refused";
                        result.error =
                            Some("The server refused this key. Fix key, then try again.".into());
                    }
                    Err(_) => {
                        result.source = "unavailable";
                        result.error = Some("Couldn't reach the server. Hosted publications could not be checked. Try again.".into());
                    }
                }
            } else {
                result.source = "unavailable";
                result.error = Some("This build has no dashboard host.".into());
            }
        }
    }
    result.rows = rows.into_values().collect();
    for team_row in team_rows(roots) {
        let team_folder = team_row
            .folder
            .as_deref()
            .map(Path::new)
            .map(state::folder_key);
        let same_team = |row: &InventoryRow| {
            row.team
                .as_ref()
                .zip(team_row.team.as_ref())
                .is_some_and(|(a, b)| {
                    a.org == b.org
                        && a.team == b.team
                        && a.shared_from == b.shared_from
                        && a.relative_folder == b.relative_folder
                })
        };
        let same_folder = |row: &InventoryRow| {
            row.folder.as_deref().map(Path::new).map(state::folder_key) == team_folder
        };
        if let Some(existing) = result
            .rows
            .iter_mut()
            .find(|row| same_folder(row) && same_team(row))
        {
            if existing.slug == team_row.slug {
                existing.title = team_row.title;
            }
        } else if let Some(existing) = result
            .rows
            .iter_mut()
            .find(|row| same_folder(row) && row.team.is_none())
        {
            existing.team = team_row.team;
            if existing.slug == team_row.slug {
                existing.title = team_row.title;
            }
        } else {
            let duplicate = result.rows.iter().find(|row| same_folder(row)).cloned();
            if let Some(mut duplicate) = duplicate {
                duplicate.team = team_row.team;
                if duplicate.slug == team_row.slug {
                    duplicate.title = team_row.title;
                }
                result.rows.push(duplicate);
            } else {
                result.rows.push(team_row);
            }
        }
    }
    result.rows.sort_by(|a, b| {
        (
            &a.slug,
            &a.folder,
            &a.team.as_ref().map(|team| (&team.org, &team.team)),
        )
            .cmp(&(
                &b.slug,
                &b.folder,
                &b.team.as_ref().map(|team| (&team.org, &team.team)),
            ))
    });
    Ok(result)
}

fn team_rows(roots: &[IndexRoot]) -> Vec<InventoryRow> {
    let mut rows = Vec::new();
    for root in roots
        .iter()
        .filter(|root| root.layer == Layer::Team && is_team_content_root(root))
    {
        let (Some(org), Some(team)) = (&root.org, &root.team) else {
            continue;
        };
        let mut add_manifest =
            |path: &Path, shared_from: Option<String>, relative_folder: String| {
                let Some(folder) = path.parent() else { return };
                let Ok(manifest) = manifest::load(folder) else {
                    return;
                };
                rows.push(InventoryRow {
                    slug: manifest.slug,
                    title: manifest.title,
                    folder: Some(state::folder_key(folder)),
                    location: "local",
                    local_state: None,
                    hosted: None,
                    pushed_at: None,
                    checked_at: None,
                    error: None,
                    different_key: false,
                    team: Some(InventoryTeam {
                        org: org.clone(),
                        team: team.clone(),
                        shared_from,
                        relative_folder,
                    }),
                });
            };

        let dashboards = root.dir.join("dashboards");
        for entry in walkdir::WalkDir::new(dashboards)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| entry.file_name() == manifest::MANIFEST_FILE)
        {
            let Some(relative) = entry
                .path()
                .strip_prefix(&root.dir)
                .ok()
                .and_then(|p| p.to_str())
            else {
                continue;
            };
            if !is_dashboard_manifest(relative, "dashboards/") {
                continue;
            }
            let Some(relative_folder) = relative.strip_suffix("/dashboard.yml") else {
                continue;
            };
            add_manifest(entry.path(), None, relative_folder.to_owned());
        }

        if let Some(org_root) = root.dir.parent().and_then(Path::parent) {
            let view = crate::context_repo::inheritance::team_view(org_root, org, team);
            for layer in &view.inherited {
                for (relative, path) in layer.walk("content/dashboards") {
                    if is_dashboard_manifest(&relative, "content/dashboards/") {
                        let Some(relative_folder) = relative
                            .strip_prefix("content/")
                            .and_then(|path| path.strip_suffix("/dashboard.yml"))
                        else {
                            continue;
                        };
                        add_manifest(
                            &path,
                            Some(layer.team().to_owned()),
                            relative_folder.to_owned(),
                        );
                    }
                }
            }
        }
    }
    rows.sort_by(|a, b| (&a.slug, &a.folder).cmp(&(&b.slug, &b.folder)));
    rows
}

fn team_headers(roots: &[IndexRoot]) -> Vec<InventoryTeamHeader> {
    let mut teams = roots
        .iter()
        .filter(|root| root.layer == Layer::Team && is_team_content_root(root))
        .filter_map(|root| {
            Some(InventoryTeamHeader {
                org: root.org.clone()?,
                team: root.team.clone()?,
            })
        })
        .collect::<Vec<_>>();
    teams.sort_by(|a, b| (&a.org, &a.team).cmp(&(&b.org, &b.team)));
    teams.dedup_by(|a, b| a.org == b.org && a.team == b.team);
    teams
}

fn is_team_content_root(root: &IndexRoot) -> bool {
    root.dir.file_name().is_some_and(|name| name == "content")
        && root
            .dir
            .parent()
            .and_then(Path::file_name)
            .zip(root.team.as_deref())
            .is_some_and(|(name, team)| name == team)
}

fn is_dashboard_manifest(relative: &str, prefix: &str) -> bool {
    relative
        .strip_prefix(prefix)
        .and_then(|path| path.strip_suffix("/dashboard.yml"))
        .is_some_and(|folder| !folder.is_empty() && !folder.contains('/'))
}

pub(crate) async fn attach_folder(data: &Path, folder: &Path) -> Result<(), String> {
    if !folder.is_absolute() {
        return Err("Choose an absolute dashboard folder.".into());
    }
    let manifest = manifest::load(folder).map_err(|e| e.to_string())?;
    let turn = super::driver::slug_lock(&manifest.slug);
    let _guard = turn.lock().await;
    let mut record = state::load(data, &manifest.slug)?;
    if record.folder.as_ref().is_some_and(|f| {
        Path::new(f).exists() && state::folder_key(Path::new(f)) != state::folder_key(folder)
    }) {
        return Err("Another local folder already uses this dashboard slug.".into());
    }
    record.folder = Some(state::folder_key(folder));
    state::save(data, &record)?;
    Ok(())
}

#[tauri::command]
pub async fn dashboard_inventory(
    app: tauri::AppHandle,
    folder: Option<String>,
    local_only: Option<bool>,
) -> Result<Inventory, String> {
    let data = super::management::data_dir(&app)?;
    if let Some(folder) = folder {
        attach_folder(&data, Path::new(&folder)).await?;
    }
    let roots = app
        .try_state::<crate::context_query::ContextIndexService>()
        .map(|service| service.snapshot().roots.clone())
        .unwrap_or_default();
    inventory_with_teams(
        &data,
        keys::store(),
        DASH_HOST,
        &roots,
        local_only.unwrap_or(false),
    )
    .await
    .map_err(|e| crate::feedback::redact(&e))
}

#[cfg(test)]
mod tests {
    use super::super::client::fake::{Fake, Reply};
    use super::super::client::TOKEN_ACCOUNT;
    use super::*;
    use crate::commands::credential_store::{CredentialStore, FakeStore, Secret};
    #[tokio::test]
    async fn merges_hosted_rows_with_local_folders_and_marks_other_key() {
        let data = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let mut local = state::PublishRecord::new("metrics");
        local.folder = Some(folder.path().to_string_lossy().into());
        local.state = LocalState::Published;
        state::save(data.path(), &local).unwrap();
        let mut other = state::PublishRecord::new("old-key");
        other.state = LocalState::Published;
        state::save(data.path(), &other).unwrap();
        let store: Arc<dyn CredentialStore> = Arc::new(FakeStore::default());
        store
            .set(TOKEN_ACCOUNT, &Secret::new("csp_test_key_a91f"))
            .unwrap();
        let fake = Fake::start(|_| Reply::Json(200, r#"{"dashboards":[{"slug":"metrics","title":"Metrics","state":"published","visibility":"private","current_version":7},{"slug":"hosted-only","title":"Hosted","state":"published","visibility":"private","current_version":2}]}"#.into())).await;
        let result = inventory_with(data.path(), store, Some(&fake.base))
            .await
            .unwrap();
        assert_eq!(result.source, "hosted");
        assert_eq!(result.rows.len(), 3);
        assert_eq!(
            result
                .rows
                .iter()
                .find(|r| r.slug == "metrics")
                .unwrap()
                .location,
            "both"
        );
        assert!(
            result
                .rows
                .iter()
                .find(|r| r.slug == "old-key")
                .unwrap()
                .different_key
        );
        assert_eq!(fake.routes(), vec!["GET /api/dash/v1/d"]);
    }
    #[tokio::test]
    async fn missing_endpoint_is_local_only_and_offline_is_unavailable() {
        let data = tempfile::tempdir().unwrap();
        let store: Arc<dyn CredentialStore> = Arc::new(FakeStore::default());
        store
            .set(TOKEN_ACCOUNT, &Secret::new("csp_test_key_a91f"))
            .unwrap();
        let fake = Fake::start(|_| Reply::Json(404, "{}".into())).await;
        assert_eq!(
            inventory_with(data.path(), store.clone(), Some(&fake.base))
                .await
                .unwrap()
                .source,
            "unsupported"
        );
        let offline = Fake::start(|_| Reply::Drop).await;
        assert_eq!(
            inventory_with(data.path(), store, Some(&offline.base))
                .await
                .unwrap()
                .source,
            "unavailable"
        );
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    #[tokio::test]
    async fn reattaches_a_moved_source_but_keeps_two_existing_sources_distinct() {
        let data = tempfile::tempdir().unwrap();
        let source = super::super::push::tests::dashboard();
        attach_folder(data.path(), source.path()).await.unwrap();
        let moved_parent = tempfile::tempdir().unwrap();
        let moved = moved_parent.path().join("moved");
        std::fs::rename(source.path(), &moved).unwrap();
        attach_folder(data.path(), &moved).await.unwrap();
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().folder,
            Some(state::folder_key(&moved))
        );
        let other = super::super::push::tests::dashboard();
        assert!(attach_folder(data.path(), other.path()).await.is_err());
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().folder,
            Some(state::folder_key(&moved))
        );
    }
}

#[cfg(test)]
mod team_inventory_tests {
    use super::*;
    use crate::commands::credential_store::FakeStore;
    use crate::context_query::index::IndexRoot;
    use crate::context_query::model::Layer;
    use std::path::PathBuf;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn dashboard(content: &Path, folder: &str, slug: &str, title: &str) -> PathBuf {
        let dir = content.join("dashboards").join(folder);
        write(
            &dir.join("dashboard.yml"),
            &format!(
                "slug: {slug}\ntitle: {title}\npublish:\n  state: draft\n  visibility: private\n  files:\n    - index.html\n"
            ),
        );
        write(&dir.join("index.html"), "<h1>Dashboard</h1>");
        dir
    }

    fn org(root: &Path, slug: &str, teams: &str) {
        write(
            &root.join("org.yml"),
            &format!("slug: {slug}\ndisplay_name: Test\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
        );
        write(&root.join("people.yml"), "people: []\n");
        write(&root.join("teams.yml"), teams);
    }

    #[test]
    fn discovers_own_and_only_admitted_inherited_dashboards_using_manifest_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("org");
        org(
            &root,
            "acme/ctx",
            "teams:\n  - name: child\n    parent_team: parent\n  - name: parent\n",
        );
        let child = root.join("child/content");
        let parent = root.join("parent/content");
        let own = dashboard(&child, "folder-name", "riverside-workspaces", "Own");
        let admitted = dashboard(&parent, "exported", "parent-dashboard", "Exported");
        let _private = dashboard(&parent, "private", "private-dashboard", "Private");
        write(
            &root.join("parent/config/sharing.yml"),
            "share:\n  - content/dashboards/exported/dashboard.yml\n  - content/dashboards/exported/index.html\n",
        );
        let roots = [IndexRoot {
            dir: child,
            org: Some("acme/ctx".into()),
            team: Some("child".into()),
            layer: Layer::Team,
        }];

        let rows = team_rows(&roots);

        let own_folder_key = state::folder_key(&own);
        assert_eq!(rows.len(), 2);
        let own_row = rows
            .iter()
            .find(|r| r.slug == "riverside-workspaces")
            .unwrap();
        assert_eq!(own_row.title, "Own");
        assert_eq!(own_row.folder.as_deref(), Some(own_folder_key.as_str()));
        assert_eq!(own_row.team.as_ref().unwrap().shared_from, None);
        let inherited_row = rows.iter().find(|r| r.slug == "parent-dashboard").unwrap();
        let admitted_folder_key = state::folder_key(&admitted);
        assert_eq!(
            inherited_row.folder.as_deref(),
            Some(admitted_folder_key.as_str())
        );
        assert_eq!(
            inherited_row.team.as_ref().unwrap().shared_from.as_deref(),
            Some("parent")
        );
        assert_eq!(
            inherited_row.team.as_ref().unwrap().relative_folder,
            "dashboards/exported"
        );
        assert!(!rows.iter().any(|r| r.slug == "private-dashboard"));
    }

    #[test]
    fn keeps_same_slug_rows_for_distinct_team_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root_a = tmp.path().join("a");
        let root_b = tmp.path().join("b");
        org(&root_a, "acme/a", "teams:\n  - name: team-a\n");
        org(&root_b, "acme/b", "teams:\n  - name: team-b\n");
        let folder_a = dashboard(&root_a.join("team-a/content"), "one", "collision", "One");
        let folder_b = dashboard(&root_b.join("team-b/content"), "two", "collision", "Two");
        let roots = [
            IndexRoot {
                dir: root_a.join("team-a/content"),
                org: Some("acme/a".into()),
                team: Some("team-a".into()),
                layer: Layer::Team,
            },
            IndexRoot {
                dir: root_b.join("team-b/content"),
                org: Some("acme/b".into()),
                team: Some("team-b".into()),
                layer: Layer::Team,
            },
        ];

        let rows = team_rows(&roots);

        assert_eq!(rows.iter().filter(|r| r.slug == "collision").count(), 2);
        assert!(rows
            .iter()
            .any(|r| r.folder.as_deref() == Some(&state::folder_key(&folder_a))));
        assert!(rows
            .iter()
            .any(|r| r.folder.as_deref() == Some(&state::folder_key(&folder_b))));
    }

    #[tokio::test]
    async fn inventory_merges_same_folder_record_without_writing_records() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("org");
        org(&root, "acme/ctx", "teams:\n  - name: riverside\n");
        let content = root.join("riverside/content");
        let folder = dashboard(
            &content,
            "different-folder",
            "riverside-workspaces",
            "Riverside",
        );
        let roots = [IndexRoot {
            dir: content,
            org: Some("acme/ctx".into()),
            team: Some("riverside".into()),
            layer: Layer::Team,
        }];
        let mut record = state::PublishRecord::new("riverside-workspaces");
        record.folder = Some(folder.to_string_lossy().into_owned());
        state::save(tmp.path(), &record).unwrap();
        let before = state::list(tmp.path()).unwrap();

        let result = inventory_with_teams(
            tmp.path(),
            Arc::new(FakeStore::default()),
            None,
            &roots,
            false,
        )
        .await
        .unwrap();

        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].folder.as_deref(),
            Some(folder.to_str().unwrap())
        );
        assert_eq!(result.rows[0].team.as_ref().unwrap().team, "riverside");
        assert_eq!(state::list(tmp.path()).unwrap(), before);
    }

    #[tokio::test]
    async fn inventory_keeps_parent_and_child_membership_for_one_inherited_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("org");
        org(
            &root,
            "acme/context",
            "teams:\n  - name: child\n    parent_team: parent\n  - name: parent\n",
        );
        let folder = dashboard(&root.join("parent/content"), "shared", "metrics", "Metrics");
        write(
            &root.join("parent/config/sharing.yml"),
            "share:\n  - content/dashboards/shared/dashboard.yml\n  - content/dashboards/shared/index.html\n",
        );
        let roots = [
            IndexRoot {
                dir: root.join("parent/content"),
                org: Some("acme/context".into()),
                team: Some("parent".into()),
                layer: Layer::Team,
            },
            IndexRoot {
                dir: root.join("child/content"),
                org: Some("acme/context".into()),
                team: Some("child".into()),
                layer: Layer::Team,
            },
        ];

        let result = inventory_with_teams(
            tmp.path(),
            Arc::new(FakeStore::default()),
            None,
            &roots,
            true,
        )
        .await
        .unwrap();
        let rows = result
            .rows
            .iter()
            .filter(|row| row.folder.as_deref() == Some(state::folder_key(&folder).as_str()))
            .collect::<Vec<_>>();

        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .any(|row| row.team.as_ref().is_some_and(|team| team.team == "parent")));
        assert!(rows
            .iter()
            .any(|row| row.team.as_ref().is_some_and(|team| team.team == "child")));
    }

    #[tokio::test]
    async fn local_only_inventory_does_not_fetch_the_hosted_list() {
        use super::super::client::fake::{Fake, Reply};
        use super::super::client::TOKEN_ACCOUNT;
        use crate::commands::credential_store::{CredentialStore, Secret};

        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(FakeStore::default());
        store
            .set(TOKEN_ACCOUNT, &Secret::new("csp_test_key_a91f"))
            .unwrap();
        let server = Fake::start(|_| Reply::Json(200, r#"{"dashboards":[]}"#.into())).await;

        let result = inventory_with_teams(tmp.path(), store, Some(&server.base), &[], true)
            .await
            .unwrap();

        assert_eq!(result.source, "local");
        assert!(server.routes().is_empty());
    }
}
