// SPDX-License-Identifier: MIT
use std::fs;
use std::path::PathBuf;

use tauri::Manager;

use crate::commands::async_helpers::await_blocking;

fn app_data_dir<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve app data dir: {}", e))?;
    fs::create_dir_all(&data_dir).map_err(|e| format!("failed to create app data dir: {}", e))?;
    Ok(data_dir)
}

// SW-MCP-11: `pub(crate)` so the summary-heading reader can locate app-state.
pub(crate) fn app_state_path<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<PathBuf, String> {
    Ok(app_data_dir(app)?.join("app-state.json"))
}

/// SW-MCP-11: synchronously read the configured summary-section headings from
/// app-state, resolved through the SAME `pipeline_scanner::effective_headings`
/// the pipeline viewer uses (config value → trimmed/deblanked → fallback to
/// `["Purpose","Story"]`). The MCP authoring tools (`get_story_format` /
/// `validate_story`) call this so their guidance matches what the pipeline card
/// actually renders — never a drifting second copy of the default.
///
/// SYNC by necessity: `get_story_format` is a sync MCP handler and cannot
/// `.await` the async `load_app_state`. NEVER errors — any failure (missing
/// file, unreadable, malformed JSON, absent/empty/wrong-typed key) resolves to
/// the defaults, so authoring guidance degrades gracefully rather than failing
/// the tool call (story AC-8).
///
/// `viewer.settings.summaryHeadings` is a GLOBAL scalar in `app-state.json`
/// (the same value the FE reads and passes to `scan_pipeline_tree`), so reading
/// it here is the same source of truth — modulo the debounced-persist lag,
/// which is acceptable for authoring guidance.
pub(crate) fn read_configured_summary_headings<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Vec<String> {
    // Missing file / unreadable path → treat as empty app-state; the parser then
    // resolves to defaults. `"{}"` is the same fallback `load_app_state` uses.
    let json = app_state_path(app)
        .ok()
        .filter(|p| p.exists())
        .and_then(|p| fs::read_to_string(&p).ok())
        .unwrap_or_else(|| "{}".to_string());
    parse_configured_summary_headings(&json)
}

/// Pure `app-state JSON → effective headings` — split out so the plucking +
/// fallback logic is unit-testable without an `AppHandle`. Malformed JSON,
/// an absent/wrong-typed `settings.summaryHeadings`, or an all-blank list all
/// resolve to the defaults via `effective_headings` (never an error).
fn parse_configured_summary_headings(json: &str) -> Vec<String> {
    let configured: Vec<String> = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            // On disk the viewer store owns its slice under a top-level `viewer`
            // key, so the setting lives at `viewer.settings.summaryHeadings`
            // (see `viewer-store.ts` `existing.viewer = { … settings … }`), NOT
            // a top-level `settings`.
            v.get("viewer")
                .and_then(|vw| vw.get("settings"))
                .and_then(|s| s.get("summaryHeadings"))
                .and_then(|h| h.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
        })
        .unwrap_or_default();
    crate::commands::pipeline_scanner::effective_headings(configured)
}

/// Resolve the contexts dir path without creating it (or its parent). Single
/// source of truth for the `<app-data>/contexts` location so read-only callers
/// (e.g. the MCP repo count) don't re-derive — and can't drift onto — the path.
/// [`contexts_dir`] layers directory creation on top for write callers.
pub fn contexts_dir_path<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve app data dir: {}", e))?;
    Ok(data_dir.join("contexts"))
}

pub fn contexts_dir<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    let dir = contexts_dir_path(app)?;
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create contexts dir: {}", e))?;
    Ok(dir)
}

#[tauri::command]
pub async fn ensure_directory(path: String) -> Result<(), String> {
    await_blocking("ensure_directory", move || {
        fs::create_dir_all(&path).map_err(|e| format!("failed to create directory {}: {}", path, e))
    })
    .await
}

pub(crate) mod profile_workspace;

pub(crate) fn preserve_profile_workspace(
    app: &tauri::AppHandle,
    principal: Option<&crate::domain::Principal>,
) -> Result<(), String> {
    profile_workspace::preserve_legacy(
        &app_state_path(app)?,
        principal.map(crate::domain::Principal::as_str),
    )
}

#[cfg(test)]
fn save_profile_state(
    profile: &super::local_profile::LocalProfileState,
    path: &std::path::Path,
    expected: Option<&str>,
    state: &str,
) -> Result<(), String> {
    let expected = expected.ok_or("Choose a local profile before saving workspace state")?;
    profile.with_expected(Some(expected), || {
        profile_workspace::save(path, expected, state)
    })
}

#[tauri::command]
pub async fn save_app_state(
    app: tauri::AppHandle,
    state: String,
    expected_principal: Option<String>,
    patch: Option<bool>,
) -> Result<(), String> {
    await_blocking("save_app_state", move || {
        write_app_state(
            &app,
            &app_state_path(&app)?,
            expected_principal.as_deref(),
            &state,
            patch.unwrap_or(false),
        )
    })
    .await
}

pub(crate) fn write_app_state<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    path: &std::path::Path,
    expected: Option<&str>,
    state: &str,
    patch: bool,
) -> Result<(), String> {
    let principal = expected.ok_or("Choose a local profile before saving workspace state")?;
    app.state::<super::local_profile::LocalProfileState>()
        .with_expected(Some(principal), || {
            let value = profile_workspace::save_value(path, principal, state, patch)?;
            crate::live_hub::configuration::value_changed(app, path, value.clone())?;
            let state = serde_json::to_string(&value).map_err(|e| e.to_string())?;
            if summary_headings_changed(&state) {
                crate::context_query::note_summary_headings(
                    app,
                    parse_configured_summary_headings(&state),
                );
            }
            Ok(())
        })
}

#[tauri::command]
pub async fn load_app_state<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    expected_principal: Option<String>,
) -> Result<String, String> {
    await_blocking("load_app_state", move || {
        app.state::<super::local_profile::LocalProfileState>()
            .with_expected(expected_principal.as_deref(), || {
                let state =
                    profile_workspace::load(&app_state_path(&app)?, expected_principal.as_deref())?;
                Ok(state)
            })
    })
    .await
}

/// Drop the workspace rows `matches` picks in `team_key` from the saved app
/// state, through the same load/save pair the viewer saves with, so a reload
/// does not bring the workspace back. Writes nothing when no row matched.
/// Returns how many rows were dropped.
pub(crate) fn retire_saved_workspace_rows(
    app: &tauri::AppHandle,
    principal: &str,
    team_key: &str,
    matches: &dyn Fn(&serde_json::Value) -> bool,
) -> Result<usize, String> {
    let path = app_state_path(app)?;
    app.state::<super::local_profile::LocalProfileState>()
        .with_expected(Some(principal), || {
            let saved = profile_workspace::load(&path, Some(principal))?;
            let mut state: serde_json::Value =
                serde_json::from_str(&saved).map_err(|e| e.to_string())?;
            let Some(viewer) = state.get_mut("viewer") else {
                return Ok(0);
            };
            let dropped = crate::mcp::state::retire_workspace_rows(viewer, team_key, matches);
            if dropped > 0 {
                let json = serde_json::to_string(&state).map_err(|e| e.to_string())?;
                profile_workspace::save(&path, principal, &json)?;
            }
            Ok(dropped)
        })
}

/// Returns the port the local HTTP API server is bound to, or `None` if
/// the server was disabled via `CODESENSE_DISABLE_HTTP`.
#[tauri::command]
pub fn get_http_api_port(app: tauri::AppHandle) -> Option<u16> {
    app.try_state::<crate::http::HttpApiPort>().map(|p| p.0)
}

#[tauri::command]
pub async fn get_log_file_path(app: tauri::AppHandle) -> Result<String, String> {
    await_blocking("get_log_file_path", move || {
        let log_dir = app
            .path()
            .app_log_dir()
            .map_err(|e| format!("failed to resolve log dir: {}", e))?;
        Ok(log_dir.to_string_lossy().to_string())
    })
    .await
}

/// Whether the app log keeps debug lines (`CODESENSE_LOG` or a dev build), so the
/// frontend can skip building debug-only diagnostics when they would be dropped.
#[tauri::command]
pub fn get_log_debug_enabled() -> bool {
    log::log_enabled!(log::Level::Debug)
}

/// SW-DNS-2: every raw `"summaryHeadings": [ … ]` text of a saved app state,
/// found without parsing it (`None` when absent). All matches are kept, so a
/// dashboard pref that happens to share the name can never mask a change to the
/// setting itself (a change to either just costs one parse).
fn summary_headings_fragment(state: &str) -> Option<String> {
    let found: Vec<&str> = state
        .match_indices("\"summaryHeadings\"")
        .map(|(at, _)| {
            let rest = &state[at..];
            let end = rest.find(']').map(|i| i + 1).unwrap_or(rest.len().min(256));
            &rest[..end]
        })
        .collect();
    (!found.is_empty()).then(|| found.join("\u{0}"))
}

/// SW-DNS-2: whether this save's `summaryHeadings` differ from the last save's.
fn summary_headings_changed(state: &str) -> bool {
    static LAST: std::sync::Mutex<Option<Option<String>>> = std::sync::Mutex::new(None);
    let now = summary_headings_fragment(state);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_ref() == Some(&now) {
        return false;
    }
    *last = Some(now);
    true
}

#[cfg(test)]
mod summary_headings_fragment_tests {
    use super::summary_headings_fragment;

    #[test]
    fn finds_the_list_without_parsing_and_ignores_the_rest() {
        let a = r#"{"viewer":{"openTabs":{"x":[1]},"settings":{"summaryHeadings":["Purpose","Story"],"theme":"dark"}}}"#;
        let b = r#"{"viewer":{"openTabs":{"y":[2,3]},"settings":{"summaryHeadings":["Purpose","Story"],"theme":"light"}}}"#;
        let c = r#"{"viewer":{"settings":{"summaryHeadings":["Overview"]}}}"#;
        assert_eq!(
            summary_headings_fragment(a).as_deref(),
            Some(r#""summaryHeadings":["Purpose","Story"]"#)
        );
        assert_eq!(summary_headings_fragment(a), summary_headings_fragment(b));
        assert_ne!(summary_headings_fragment(a), summary_headings_fragment(c));
        assert_eq!(summary_headings_fragment(r#"{"viewer":{}}"#), None);
    }

    /// A dashboard pref that happens to be named `summaryHeadings` (stored before
    /// the viewer settings) never hides a real change to the setting.
    #[test]
    fn a_dashboard_pref_named_summary_headings_does_not_mask_the_setting() {
        let a = r#"{"dashboardPrefs":{"doc:/d.html":{"data":{"summaryHeadings":["Mine"]}}},"viewer":{"settings":{"summaryHeadings":["Purpose","Story"]}}}"#;
        let b = r#"{"dashboardPrefs":{"doc:/d.html":{"data":{"summaryHeadings":["Mine"]}}},"viewer":{"settings":{"summaryHeadings":["Overview"]}}}"#;
        assert_ne!(summary_headings_fragment(a), summary_headings_fragment(b));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("codesense_test").join(format!(
            "{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &std::path::Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn test_ensure_directory_creates_nested() {
        let dir = test_dir("ensure_dir");
        let nested = dir.join("a").join("b").join("c");

        let result = ensure_directory(nested.display().to_string()).await;
        assert!(result.is_ok());
        assert!(nested.exists());

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_ensure_directory_noop_if_exists() {
        let dir = test_dir("ensure_dir_noop");

        let result = ensure_directory(dir.display().to_string()).await;
        assert!(result.is_ok());
        // Call again — should be no-op
        let result2 = ensure_directory(dir.display().to_string()).await;
        assert!(result2.is_ok());

        cleanup(&dir);
    }

    // ----- SW-MCP-11: configured summary-heading reader ----------------------

    fn defaults() -> Vec<String> {
        crate::commands::pipeline_scanner::default_summary_headings()
    }

    // The real on-disk shape: the viewer store nests its slice under a
    // top-level `viewer` key, so the setting is at `viewer.settings.*`.
    #[test]
    fn configured_summary_headings_read_from_viewer_settings() {
        let json =
            r#"{ "viewer": { "settings": { "summaryHeadings": ["Overview", "Summary"] } } }"#;
        assert_eq!(
            parse_configured_summary_headings(json),
            vec!["Overview".to_string(), "Summary".to_string()]
        );
    }

    #[test]
    fn top_level_settings_is_ignored_only_viewer_settings_counts() {
        // Guards the round-2 HIGH: a top-level `settings` is the WRONG path and
        // must be ignored — only `viewer.settings` is the real persisted home.
        let json = r#"{ "settings": { "summaryHeadings": ["WrongPath"] } }"#;
        assert_eq!(parse_configured_summary_headings(json), defaults());
    }

    #[test]
    fn absent_key_falls_back_to_defaults() {
        assert_eq!(parse_configured_summary_headings("{}"), defaults());
        assert_eq!(
            parse_configured_summary_headings(r#"{ "viewer": { "settings": {} } }"#),
            defaults()
        );
        assert_eq!(
            parse_configured_summary_headings(r#"{ "viewer": {} }"#),
            defaults()
        );
    }

    #[test]
    fn empty_or_whitespace_config_falls_back_to_defaults() {
        // Empty array, and blank/whitespace entries → effective_headings drops
        // them and resolves to defaults (never blanks every card).
        assert_eq!(
            parse_configured_summary_headings(
                r#"{ "viewer": { "settings": { "summaryHeadings": [] } } }"#
            ),
            defaults()
        );
        assert_eq!(
            parse_configured_summary_headings(
                r#"{ "viewer": { "settings": { "summaryHeadings": ["  ", ""] } } }"#
            ),
            defaults()
        );
    }

    #[test]
    fn malformed_json_falls_back_to_defaults_never_errors() {
        assert_eq!(parse_configured_summary_headings("not json {{"), defaults());
        // Wrong type for the key (object, not array) → defaults, no panic.
        assert_eq!(
            parse_configured_summary_headings(
                r#"{ "viewer": { "settings": { "summaryHeadings": "Purpose" } } }"#
            ),
            defaults()
        );
    }

    #[test]
    fn config_headings_are_trimmed() {
        let json = r#"{ "viewer": { "settings": { "summaryHeadings": [" Purpose ", "Story"] } } }"#;
        assert_eq!(
            parse_configured_summary_headings(json),
            vec!["Purpose".to_string(), "Story".to_string()]
        );
    }
}

#[cfg(test)]
mod profile_write_tests {
    use super::*;
    #[test]
    fn cp3_stalled_app_state_write_does_not_block_profile_readers() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("users/41")).unwrap();
        let profile =
            Arc::new(super::super::local_profile::LocalProfileState::load(home.path()).unwrap());
        let path = home.path().join("app-state.json");
        let (entered, stalled) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let writer_profile = profile.clone();
        let writer = std::thread::spawn(move || {
            profile_workspace::with_write_hook(
                move || {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                },
                || save_profile_state(&writer_profile, &path, Some("41"), "{}"),
            )
        });
        stalled.recv_timeout(Duration::from_secs(5)).unwrap();
        let (read, observed) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let active = profile.active().unwrap();
            let peek = profile.peek_active().unwrap();
            read.send((active.principal, peek.principal)).unwrap();
        });
        let result = observed.recv_timeout(Duration::from_millis(250));
        release.send(()).unwrap();
        writer.join().unwrap().unwrap();
        reader.join().unwrap();
        let (active, peek) = result.expect("profile readers waited behind app-state I/O");
        assert_eq!(active.as_str(), "41");
        assert_eq!(peek.as_str(), "41");
    }

    #[test]
    fn retired_renderer_cannot_save_workspace_or_global_preferences() {
        let home = tempfile::tempdir().unwrap();
        for id in ["41", "42"] {
            std::fs::create_dir_all(home.path().join("users").join(id)).unwrap();
        }
        let profile = super::super::local_profile::LocalProfileState::load(home.path()).unwrap();
        let path = home.path().join("app-state.json");
        profile.select("41", None).unwrap();
        save_profile_state(
            &profile,
            &path,
            Some("41"),
            r#"{"org":{"activeOrgSlug":"A"},"viewer":{"settings":{"theme":"dark"}}}"#,
        )
        .unwrap();
        profile.select("42", None).unwrap();
        let before = fs::read(&path).unwrap();
        for expected in [Some("41"), Some("042"), None] {
            assert!(save_profile_state(
                &profile,
                &path,
                expected,
                r#"{"org":{"activeOrgSlug":"stale"},"viewer":{"settings":{"theme":"stale"}}}"#
            )
            .is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        save_profile_state(
            &profile,
            &path,
            Some("42"),
            r#"{"org":{"activeOrgSlug":"B"},"viewer":{"settings":{"theme":"light"}}}"#,
        )
        .unwrap();
        let a: serde_json::Value =
            serde_json::from_str(&profile_workspace::load(&path, Some("41")).unwrap()).unwrap();
        let b: serde_json::Value =
            serde_json::from_str(&profile_workspace::load(&path, Some("42")).unwrap()).unwrap();
        assert_eq!(a["org"]["activeOrgSlug"], "A");
        assert_eq!(b["org"]["activeOrgSlug"], "B");
        assert_eq!(a["viewer"]["settings"]["theme"], "light");
    }
}

pub(crate) fn save_workspace_groups(
    app: &tauri::AppHandle,
    principal: &str,
    team: &str,
    groups: serde_json::Value,
    revision: u64,
) -> Result<(), String> {
    profile_workspace::save_groups(&app_state_path(app)?, principal, team, groups, revision)
}
#[tauri::command]
pub async fn workspace_group_edit(
    app: tauri::AppHandle,
    team_key: String,
    edit: serde_json::Value,
    expected_principal: Option<String>,
) -> Result<serde_json::Value, String> {
    super::interactive_gate::InteractiveGate::admit("workspace_group_edit")
        .map_err(|e| e.to_string())?;
    let expected = expected_principal.ok_or("Choose a local profile before editing groups")?;
    await_blocking("workspace_group_edit", move || {
        crate::mcp::workspace_groups::apply(&app, &team_key, edit, Some(&expected))
    })
    .await
}
