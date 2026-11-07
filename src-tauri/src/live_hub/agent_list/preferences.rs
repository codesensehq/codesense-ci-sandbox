// SPDX-License-Identifier: MIT
use serde_json::{json, Value};
use std::sync::Mutex;
use tauri::{Emitter, Manager};

/// Identity choices are persisted here, independent of the viewer's save debounce.
#[derive(Default)]
pub(crate) struct Preferences {
    cache: Mutex<Option<Value>>,
    writes: Mutex<()>,
}
impl Preferences {
    fn update(
        &self,
        path: &std::path::Path,
        edit: impl FnOnce(&mut Value),
    ) -> Result<Value, String> {
        let _write = crate::shared::lock_ext::lock_named(&self.writes, "agent choices I/O")
            .map_err(|_| "Agent choices unavailable")?;
        let cached = self
            .cache
            .lock()
            .map_err(|_| "Agent choices unavailable")?
            .clone();
        let mut state = match &cached {
            Some(v) => v.clone(),
            None => match std::fs::read(path) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| format!("Read agent choices: {e}"))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({"agentIdentity":{}}),
                Err(e) => return Err(e.to_string()),
            },
        };
        edit(&mut state);
        crate::content_engine::atomic_io::write_tmp_fsync_rename(
            path,
            &serde_json::to_vec(&state).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        *self.cache.lock().map_err(|_| "Agent choices unavailable")? = Some(state.clone());
        Ok(state)
    }
    pub(crate) fn identity(&self) -> Value {
        self.cache
            .lock()
            .unwrap()
            .as_ref()
            .map(|v| v["agentIdentity"].clone())
            .unwrap_or_else(|| json!({}))
    }
}
fn path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(crate::commands::system::app_state_path(app)?.with_file_name("agent-list-choices.json"))
}
#[tauri::command]
pub async fn agent_list_identity(
    app: tauri::AppHandle,
    expected_principal: Option<String>,
    legacy: Option<Value>,
) -> Result<Value, String> {
    crate::commands::await_blocking("agent_list_identity", move || {
        app.state::<crate::commands::local_profile::LocalProfileState>()
            .with_expected(expected_principal.as_deref(), || {
                let prefs = app.state::<Preferences>();
                let state = prefs.update(&path(&app)?, |state| {
                    if state.get("migrated").is_none() && legacy.is_some() {
                        if let Some(legacy) = legacy.filter(Value::is_object) {
                            state["agentIdentity"] = legacy;
                        }
                        state["migrated"] = true.into();
                    }
                })?;
                super::super::inputs_changed(&app);
                Ok(state["agentIdentity"].clone())
            })
    })
    .await
}
#[tauri::command]
pub async fn agent_list_set_identity(
    app: tauri::AppHandle,
    expected_principal: Option<String>,
    team_key: String,
    handle: String,
    choice: Option<Value>,
) -> Result<Value, String> {
    crate::commands::await_blocking("agent_list_set_identity", move || {
        app.state::<crate::commands::local_profile::LocalProfileState>()
            .with_expected(expected_principal.as_deref(), || {
                let state = app.state::<Preferences>().update(&path(&app)?, |state| {
                    let identity = state["agentIdentity"]
                        .as_object_mut()
                        .expect("identity object");
                    let team = identity
                        .entry(team_key.clone())
                        .or_insert_with(|| json!({}));
                    if let Some(choice) = choice {
                        team[&handle] = choice;
                    } else if let Some(team) = team.as_object_mut() {
                        team.remove(&handle);
                    }
                })?;
                app.emit("agent-list-identity-changed", &state["agentIdentity"])
                    .map_err(|e| e.to_string())?;
                super::super::inputs_changed(&app);
                Ok(state["agentIdentity"].clone())
            })
    })
    .await
}

#[tauri::command]
pub async fn agent_list_refresh(
    app: tauri::AppHandle,
    expected_principal: Option<String>,
) -> Result<(), String> {
    crate::commands::await_blocking("agent_list_refresh", move || {
        app.state::<crate::commands::local_profile::LocalProfileState>()
            .with_expected(expected_principal.as_deref(), || {
                super::super::refresh_sources(&app);
                Ok(())
            })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manager_choices_survive_a_new_owner_and_removal_is_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("choices.json");
        let first = Preferences::default();
        first
            .update(&path, |state| {
                state["agentIdentity"] = json!({"org:team":{"worker":{"manager":"lead"}}})
            })
            .unwrap();
        let restored = Preferences::default();
        let state = restored.update(&path, |_| {}).unwrap();
        assert_eq!(
            state["agentIdentity"]["org:team"]["worker"]["manager"],
            "lead"
        );
        restored
            .update(&path, |state| {
                state["agentIdentity"]["org:team"]
                    .as_object_mut()
                    .unwrap()
                    .remove("worker");
            })
            .unwrap();
        let reloaded = Preferences::default().update(&path, |_| {}).unwrap();
        assert!(reloaded["agentIdentity"]["org:team"]
            .get("worker")
            .is_none());
    }
}
