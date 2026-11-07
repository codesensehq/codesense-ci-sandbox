// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::commands::async_helpers::await_blocking;
use crate::shared::json_io;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRepoOverrides {
    // SW-CP-18: `viewer_root` retired. Legacy `workspace-config.json` files may
    // still carry a `viewerRoot` key; serde ignores unknown fields (no
    // `deny_unknown_fields`), so old configs deserialize cleanly without it.
    //
    // SW-XS-12 (XS-18 leftover): `override_glob_prefix` (the staleness-override
    // glob prefix SW-XS-14 retired; on disk `overrideGlobPrefix`, earlier
    // `pipelineRoot`) is gone. Older files still load: serde ignores the keys
    // (no `deny_unknown_fields`), and the next save drops them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_doc: Option<String>,
    // SW-XS-14: the per-repo staleness threshold (`viewStalenessMonths`) is
    // retired; serde ignores the key in older files on load.
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfig {
    pub repos: HashMap<String, WorkspaceRepoOverrides>,
}

fn config_path(codesense_home: &str) -> PathBuf {
    PathBuf::from(codesense_home).join("workspace-config.json")
}

// SW-WL-6-B (H8): `async` + `await_blocking` so the `fs::read_to_string` never
// runs on the UI thread (a slow / network-mounted home can't freeze the app).
// The sync `_inner` holds the logic and is exercised directly by the unit tests.
#[tauri::command]
pub async fn load_workspace_config(codesense_home: String) -> Result<WorkspaceConfig, String> {
    await_blocking("load_workspace_config", move || {
        load_workspace_config_inner(&codesense_home)
    })
    .await
}

fn load_workspace_config_inner(codesense_home: &str) -> Result<WorkspaceConfig, String> {
    let path = config_path(codesense_home);
    match json_io::read_json::<WorkspaceConfig>(&path) {
        Ok(config) => Ok(config),
        Err(json_io::JsonIoError::Io(ref e)) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(WorkspaceConfig::default())
        }
        Err(e) => Err(e.to_string()),
    }
}

// SW-WL-6-B (H7): `async` + `await_blocking` so the atomic write
// (`create_dir_all` + `fs::write` + `rename`) never runs on the UI thread.
#[tauri::command]
pub async fn save_workspace_config(
    app: tauri::AppHandle,
    codesense_home: String,
    config: serde_json::Value,
    expected_principal: Option<String>,
) -> Result<(), String> {
    crate::commands::await_blocking("save_workspace_config", move || {
        use tauri::Manager;
        app.state::<crate::commands::local_profile::LocalProfileState>()
            .with_expected(expected_principal.as_deref(), || {
                save_patch(&app, std::path::Path::new(&codesense_home), config)
            })
    })
    .await
}

pub(crate) fn save_patch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &std::path::Path,
    patch: serde_json::Value,
) -> Result<(), String> {
    static WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _write = WRITES.lock().map_err(|e| e.to_string())?;
    let path = home.join("workspace-config.json");
    let mut value: serde_json::Value = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({"repos":{}}),
        Err(e) => return Err(e.to_string()),
    };
    let repos = patch["repos"]
        .as_object()
        .ok_or("workspace patch requires repos")?;
    for (id, fields) in repos {
        if !value["repos"][id].is_object() {
            value["repos"][id] = serde_json::json!({});
        }
        for (field, next) in fields
            .as_object()
            .ok_or("workspace override must be an object")?
        {
            value["repos"][id][field] = next.clone();
        }
    }
    json_io::write_json_atomic(&path, &value).map_err(|e| e.to_string())?;
    crate::live_hub::configuration::value_changed(app, &path, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn save_workspace_config_inner(home: &str, config: &WorkspaceConfig) -> Result<(), String> {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        save_patch(
            app.handle(),
            std::path::Path::new(home),
            serde_json::to_value(config).unwrap(),
        )
    }

    // SW-WL-6-B: tests exercise the sync `_inner` helpers directly (all disk-IO
    // logic lives there); the `#[tauri::command]` faces are trivial
    // `await_blocking` wrappers, untested by design like the rest of the M1 set.
    #[test]
    fn load_missing_returns_empty() {
        let dir = tempdir().unwrap();
        let result = load_workspace_config_inner(&dir.path().to_string_lossy()).unwrap();
        assert!(result.repos.is_empty());
    }

    #[test]
    fn round_trip() {
        let dir = tempdir().unwrap();
        let home = dir.path().to_string_lossy().to_string();

        let mut config = WorkspaceConfig::default();
        config.repos.insert(
            "workspace:acme:default:org/repo".to_string(),
            WorkspaceRepoOverrides {
                main_doc: Some("Epic.md".to_string()),
            },
        );

        save_workspace_config_inner(&home, &config).unwrap();
        let loaded = load_workspace_config_inner(&home).unwrap();

        assert_eq!(loaded.repos.len(), 1);
        let entry = loaded.repos.get("workspace:acme:default:org/repo").unwrap();
        assert_eq!(entry.main_doc.as_deref(), Some("Epic.md"));
    }

    /// SW-XS-12 (XS-18 leftover): a file written while the override glob
    /// prefix existed (either spelling) still loads, keeps its other
    /// settings, and field-patch persistence keeps unrelated on-disk fields.
    #[test]
    fn load_ignores_the_retired_override_glob_prefix_keys() {
        let dir = tempdir().unwrap();
        let home = dir.path().to_string_lossy().to_string();
        std::fs::write(
            config_path(&home),
            r#"{"repos":{"a":{"mainDoc":"README.md","overrideGlobPrefix":"impl"},"b":{"pipelineRoot":"_bmad-output"}}}"#,
        )
        .unwrap();
        let loaded = load_workspace_config_inner(&home).unwrap();
        assert_eq!(loaded.repos["a"].main_doc.as_deref(), Some("README.md"));
        assert_eq!(loaded.repos["b"].main_doc, None);
        save_workspace_config_inner(&home, &loaded).unwrap();
        let text = std::fs::read_to_string(config_path(&home)).unwrap();
        assert!(
            text.contains("overrideGlobPrefix") && text.contains("pipelineRoot"),
            "{text}"
        );
    }

    /// SW-XS-14: a file written before the threshold was retired still loads.
    #[test]
    fn load_ignores_the_retired_threshold_key() {
        let dir = tempdir().unwrap();
        let home = dir.path().to_string_lossy().to_string();
        std::fs::write(
            config_path(&home),
            r#"{"repos":{"k":{"mainDoc":"README.md","viewStalenessMonths":12}}}"#,
        )
        .unwrap();
        let loaded = load_workspace_config_inner(&home).unwrap();
        assert_eq!(loaded.repos["k"].main_doc.as_deref(), Some("README.md"));
    }

    #[test]
    fn save_overwrites_existing() {
        let dir = tempdir().unwrap();
        let home = dir.path().to_string_lossy().to_string();

        let mut c1 = WorkspaceConfig::default();
        c1.repos.insert(
            "workspace:acme:default:org/repo".to_string(),
            WorkspaceRepoOverrides {
                main_doc: Some("old".to_string()),
            },
        );
        save_workspace_config_inner(&home, &c1).unwrap();

        let mut c2 = WorkspaceConfig::default();
        c2.repos.insert(
            "workspace:acme:default:org/repo".to_string(),
            WorkspaceRepoOverrides {
                main_doc: Some("new".to_string()),
            },
        );
        save_workspace_config_inner(&home, &c2).unwrap();

        let loaded = load_workspace_config_inner(&home).unwrap();
        let entry = loaded.repos.get("workspace:acme:default:org/repo").unwrap();
        assert_eq!(entry.main_doc.as_deref(), Some("new"));
    }
}
