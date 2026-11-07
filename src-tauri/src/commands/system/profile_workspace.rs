// SPDX-License-Identifier: MIT
//! Only workspace choices are profile scoped. Global preferences stay at their old paths.
use serde_json::{Map, Value};
use std::{path::Path, sync::Mutex};
static IO: Mutex<()> = Mutex::new(());
const STORAGE: &str = "localProfileWorkspacesV1";
const LEGACY: &str = "unassignedLegacyWorkspaceV1";
const UNATTRIBUTED: &str = "legacyWorkspaceUnattributedV1";
const ORG: &[&str] = &[
    "activeOrgSlug",
    "activeTeamName",
    "lastTeamByOrgSlug",
    "shownTeamsByOrgSlug",
    "disabledOrgSlugs",
];
// SW-DNS-2: `dashboardPrefs` holds each dashboard's own settings (`codesense.prefs`);
// a profile's dashboards never see another profile's.
const TOP: &[&str] = &[
    "org",
    "repos",
    "worktreeSetupApprovals",
    "dashboardPrefs",
    "dashboardPolicy",
];
const VIEWER: &[&str] = &[
    "schemaVersion",
    "openTabs",
    "activeTab",
    "drillPaths",
    "teamPipelineRepoPath",
    // The CLI's chrome, one value for every team: shown, layout, split, expanded, Agents dock.
    "cliChrome",
    "navCollapsed",
    // The Agents pane's folded teams and workspaces, the teams it hides, and its arrow filter on
    // one workspace.
    "agentsPaneFolds",
    "agentsPaneHiddenTeams",
    "agentsWorkspaceFilter",
    "foregroundLayout",
    "foregroundLayoutByRail",
    "trayMode",
    "rightPaneSplitRatio",
    "rightPaneViewMode",
    "boardSprintFilter",
    "pipelineSprintFilter",
    "boardAssigneeFilter",
    "boardStructureScope",
    "boardSwimlanesOn",
    "boardLaneCollapse",
    "statusNavExpandedPaths",
    "pipelineScope",
    // SW-XS-15: the left-nav context pivot ("Context for ‹Agent› · in ‹Repo›") is a
    // workspace choice. It replaced `selectedSourceRepo` / `selectedContextId`.
    "selectedAgentContext",
    "workspaces",
    "softClosedWorkspaces",
    // The shared order of a team's workspaces (tray pills and Agents pane cards) goes with them.
    "workspaceOrderByTeam",
    "customGroupsByTeam",
    "workspaceGroupVersionsByTeam",
    // The computed tray order the shared order applies over; it only makes sense beside it.
    "trayBaseOrderByTeam",
    "activeWorkspace",
    // This viewer's focused remote tab travels with the profile's workspace focus.
    "terminalViewerFocus",
    // Remote cells and divider trees use workspace keys, so they follow the profile too.
    "terminalViewerGrid",
    "terminalTabGridSizes",
    "openedTeams",
];
// Pre-v15 saves still need these fields attributed to their original profile
// before the frontend's one-time cliChrome seed. They are never global settings.
const LEGACY_VIEWER: &[&str] = &[
    "terminalVisible",
    "layoutMode",
    "lastTerminalMode",
    "terminalSplitRatio",
    "previousSplitRatio",
    "workspacePanelCollapsed",
    "workspacePanelWidth",
    "cliTeamKey",
];
fn read(path: &Path) -> Result<Value, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("Invalid saved workspace state: {e}"))?;
            if !value.is_object() {
                return Err("Saved workspace state must be an object".into());
            }
            for key in [STORAGE, LEGACY] {
                if value.get(key).is_some_and(|v| !v.is_object()) {
                    return Err(format!("Invalid saved workspace section: {key}"));
                }
            }
            if let Some(profiles) = value.get(STORAGE).and_then(Value::as_object) {
                for (principal, workspace) in profiles {
                    crate::domain::Principal::parse_local(principal)
                        .map_err(|_| "Invalid saved workspace profile key".to_string())?;
                    if !workspace.is_object() {
                        return Err("Invalid saved workspace profile entry".into());
                    }
                }
            }
            Ok(value)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(e) => Err(e.to_string()),
    }
}
#[cfg(test)]
thread_local! {
    static WRITE_HOOK: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn with_write_hook<T>(hook: impl Fn() + 'static, run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            WRITE_HOOK.with(|hook| {
                hook.replace(None);
            });
        }
    }
    WRITE_HOOK.with(|slot| {
        slot.replace(Some(Box::new(hook)));
    });
    let _reset = Reset;
    run()
}

fn write(path: &Path, value: &Value) -> Result<(), String> {
    #[cfg(test)]
    WRITE_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook();
        }
    });
    crate::content_engine::atomic_io::write_tmp_plain_fsync_rename(
        path,
        &serde_json::to_vec(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
fn take_workspace(value: &mut Value) -> Value {
    let mut scoped = Map::new();
    let object = value.as_object_mut().expect("validated object");
    for key in TOP {
        if let Some(value) = object.remove(*key) {
            scoped.insert((*key).into(), value);
        }
    }
    if let Some(viewer) = object.get_mut("viewer").and_then(Value::as_object_mut) {
        let mut fields = Map::new();
        for key in VIEWER.iter().chain(LEGACY_VIEWER.iter()) {
            if let Some(value) = viewer.remove(*key) {
                fields.insert((*key).into(), value);
            }
        }
        if !fields.is_empty() {
            scoped.insert("viewer".into(), Value::Object(fields));
        }
    }
    if let Some(org) = scoped.get_mut("org").and_then(Value::as_object_mut) {
        org.retain(|key, _| ORG.contains(&key.as_str()));
    }
    Value::Object(scoped)
}
// Older profile-aware versions left remote grids global. Attribute those fields
// once to the source/active profile before switching, preserving an already
// scoped value. Without a principal retain the normal unassigned legacy policy.
fn migrate_global_grids(value: &mut Value, principal: Option<&str>) -> bool {
    let mut grids = Map::new();
    if let Some(viewer) = value.get_mut("viewer").and_then(Value::as_object_mut) {
        for key in ["terminalViewerGrid", "terminalTabGridSizes"] {
            if let Some(field) = viewer.remove(key) {
                grids.insert(key.into(), field);
            }
        }
    }
    if grids.is_empty() {
        return false;
    }
    let destination = if let Some(principal) = principal {
        value[STORAGE]
            .as_object_mut()
            .expect("validated storage")
            .entry(principal)
            .or_insert_with(|| serde_json::json!({}))
    } else {
        value
            .as_object_mut()
            .expect("validated object")
            .entry(LEGACY)
            .or_insert_with(|| serde_json::json!({}))
    };
    let viewer = destination
        .as_object_mut()
        .expect("validated workspace")
        .entry("viewer")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(viewer) = viewer.as_object_mut() {
        for (key, field) in grids {
            viewer.entry(key).or_insert(field);
        }
    }
    true
}

fn prepare(value: &mut Value, principal: Option<&str>) -> bool {
    let mut changed = false;
    if value.get(STORAGE).is_none() {
        changed = true;
        let legacy = take_workspace(value);
        value[STORAGE] = serde_json::json!({});
        if !legacy.as_object().expect("object").is_empty() {
            value[LEGACY] = legacy;
        }
    }
    // An unassigned snapshot is retained when there is no selected namespace.
    // It can be attributed once, before a switch publishes its destination.
    if let Some(principal) = principal {
        if value.get(UNATTRIBUTED).is_none()
            && value.get(LEGACY).is_some()
            && value[STORAGE].get(principal).is_none()
        {
            changed = true;
            let legacy = value
                .as_object_mut()
                .expect("object")
                .remove(LEGACY)
                .expect("present");
            value[STORAGE][principal] = legacy;
        }
    }
    // A grid-only entry must not prevent an eligible legacy workspace from
    // being attributed above. Merge fields only after that workspace is known.
    let grids_changed = migrate_global_grids(value, principal);
    changed || grids_changed
}
pub(super) fn preserve_legacy(path: &Path, principal: Option<&str>) -> Result<(), String> {
    let _io = crate::shared::lock_ext::lock_named(&IO, "app-state I/O")
        .map_err(|_| "Workspace state unavailable")?;
    let mut value = read(path)?;
    prepare(&mut value, principal);
    if principal.is_none() && value.get(LEGACY).is_some() {
        value[UNATTRIBUTED] = Value::Bool(true);
    }
    write(path, &value)
}
pub(super) fn load(path: &Path, principal: Option<&str>) -> Result<String, String> {
    let _io = crate::shared::lock_ext::lock_named(&IO, "app-state I/O")
        .map_err(|_| "Workspace state unavailable")?;
    let mut value = read(path)?;
    if prepare(&mut value, principal) {
        write(path, &value)?;
    }
    let scoped = principal.and_then(|p| value[STORAGE].get(p)).cloned();
    let object = value.as_object_mut().expect("validated object");
    object.remove(STORAGE);
    object.remove(LEGACY);
    object.remove(UNATTRIBUTED);
    if let Some(Value::Object(scoped)) = scoped {
        for (key, fields) in scoped {
            if key == "viewer" {
                let viewer = object.entry(key).or_insert_with(|| serde_json::json!({}));
                if let (Some(dst), Some(src)) = (viewer.as_object_mut(), fields.as_object()) {
                    dst.extend(src.clone());
                }
            } else {
                object.insert(key, fields);
            }
        }
    }
    serde_json::to_string(&value).map_err(|e| e.to_string())
}
pub(super) fn save(path: &Path, principal: &str, incoming: &str) -> Result<(), String> {
    save_value(path, principal, incoming, false).map(|_| ())
}

pub(super) fn save_value(
    path: &Path,
    principal: &str,
    incoming: &str,
    patch: bool,
) -> Result<Value, String> {
    let mut incoming: Value = serde_json::from_str(incoming).map_err(|e| e.to_string())?;
    if !incoming.is_object() {
        return Err("Workspace state must be an object".into());
    }
    let _io = crate::shared::lock_ext::lock_named(&IO, "app-state I/O")
        .map_err(|_| "Workspace state unavailable")?;
    let mut disk = read(path)?;
    prepare(&mut disk, Some(principal));
    if incoming.get("viewer").is_none() {
        incoming["viewer"] = serde_json::json!({});
    }
    let mut workspace = take_workspace(&mut incoming);
    if patch {
        let mut merged = disk[STORAGE][principal].clone();
        merge_fields(&mut merged, &workspace);
        workspace = merged;
    }
    // Reconciliation accepts a complete snapshot, after patch deletions resolve.
    let mut viewer = workspace
        .get("viewer")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    crate::mcp::workspace_groups::reconcile_snapshot(
        &disk[STORAGE][principal]["viewer"],
        &mut viewer,
    );
    if viewer.as_object().is_some_and(|fields| !fields.is_empty()) {
        workspace["viewer"] = viewer;
    }
    disk[STORAGE][principal] = workspace;
    for (key, value) in incoming.as_object().expect("validated object") {
        if key != STORAGE && key != LEGACY && key != UNATTRIBUTED {
            if patch {
                merge_fields(&mut disk[key], value);
            } else {
                disk[key] = value.clone();
            }
        }
    }
    write(path, &disk)?;
    Ok(disk)
}

fn merge_fields(destination: &mut Value, patch: &Value) {
    if let Some(fields) = patch.as_object() {
        if !destination.is_object() {
            *destination = serde_json::json!({});
        }
        for (key, value) in fields {
            if value.is_null() {
                destination.as_object_mut().unwrap().remove(key);
            } else {
                merge_fields(&mut destination[key], value);
            }
        }
    } else {
        *destination = patch.clone();
    }
}

/// Initialize the existing workspace namespace under the same lock as load/save.
pub(crate) fn read_prefs_source(path: &Path, principal: &str) -> Result<Value, String> {
    let _io = IO.lock().map_err(|_| "Workspace state unavailable")?;
    let mut value = read(path)?;
    let before = value.clone();
    prepare(&mut value, Some(principal));
    if value != before {
        write(path, &value)?;
    }
    Ok(value)
}

/// Read-only projection: an observation cannot claim an unassigned legacy workspace.
pub(crate) fn prefs_view(value: &Value, principal: Option<&str>) -> Value {
    let mut disk = value.clone();
    if !disk.is_object() {
        disk = serde_json::json!({});
    }
    prepare(&mut disk, None);
    if let Some(principal) = principal {
        disk[STORAGE][principal]
            .as_object()
            .cloned()
            .map(Value::Object)
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        let mut global = serde_json::json!({});
        for key in ["viewer", "shortcuts"] {
            if let Some(value) = disk.get(key) {
                global[key] = value.clone();
            }
        }
        global
    }
}

pub(super) fn save_groups(
    path: &Path,
    principal: &str,
    team: &str,
    groups: Value,
    revision: u64,
) -> Result<(), String> {
    let _io = crate::shared::lock_ext::lock_named(&IO, "app-state I/O")
        .map_err(|_| "Workspace state unavailable")?;
    let mut disk = read(path)?;
    prepare(&mut disk, Some(principal));
    if !disk[STORAGE][principal].is_object() {
        disk[STORAGE][principal] = serde_json::json!({});
    }
    if !disk[STORAGE][principal]["viewer"].is_object() {
        disk[STORAGE][principal]["viewer"] = serde_json::json!({});
    }
    if !disk[STORAGE][principal]["viewer"]["customGroupsByTeam"].is_object() {
        disk[STORAGE][principal]["viewer"]["customGroupsByTeam"] = serde_json::json!({});
    }
    disk[STORAGE][principal]["viewer"]["customGroupsByTeam"][team] = groups;
    if !disk[STORAGE][principal]["viewer"]["workspaceGroupVersionsByTeam"].is_object() {
        disk[STORAGE][principal]["viewer"]["workspaceGroupVersionsByTeam"] = serde_json::json!({});
    }
    disk[STORAGE][principal]["viewer"]["workspaceGroupVersionsByTeam"][team] =
        serde_json::json!(revision);
    write(path, &disk)
}
#[cfg(test)]
mod tests {
    #[test]
    fn closing_a_team_with_a_field_patch_removes_its_versioned_groups() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(
            &path,
            "41",
            r#"{"viewer":{"openedTeams":["a:one","b:two"],"workspaces":{"a:one":[],"b:two":[]}}}"#,
        )
        .unwrap();
        let groups = serde_json::json!([{"id":"g","name":"Checks","workspaceIds":[]}]);
        save_groups(&path, "41", "a:one", groups.clone(), 7).unwrap();
        save_groups(&path, "41", "b:two", groups.clone(), 9).unwrap();

        save_value(&path, "41", r#"{"viewer":{"openedTeams":["b:two"],"workspaces":{"a:one":null},"customGroupsByTeam":{"a:one":null},"workspaceGroupVersionsByTeam":{"a:one":null}}}"#, true).unwrap();

        let restored: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        for field in [
            "workspaces",
            "customGroupsByTeam",
            "workspaceGroupVersionsByTeam",
        ] {
            assert!(
                restored["viewer"][field].get("a:one").is_none(),
                "closed team survives in {field}"
            );
        }
        assert_eq!(restored["viewer"]["customGroupsByTeam"]["b:two"], groups);
        assert_eq!(
            restored["viewer"]["workspaceGroupVersionsByTeam"]["b:two"],
            9
        );
    }

    #[test]
    fn custom_groups_are_profile_scoped_and_stale_renderer_saves_cannot_erase_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let groups =
            serde_json::json!([{"id":"g","name":"Release checks","workspaceIds":["closed-story"]}]);
        save_groups(&path, "41", "a:one", groups.clone(), 7).unwrap();
        save(&path,"41",r#"{"viewer":{"openedTeams":["a:one"],"customGroupsByTeam":{"a:one":[]},"workspaceGroupVersionsByTeam":{"a:one":6}}}"#).unwrap();
        let first: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(first["viewer"]["customGroupsByTeam"]["a:one"], groups);
        let other: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        assert!(other["viewer"].get("customGroupsByTeam").is_none());
        assert!(other["viewer"]
            .get("workspaceGroupVersionsByTeam")
            .is_none());
        save(
            &path,
            "42",
            r#"{"viewer":{"customGroupsByTeam":{"b:two":[]}}}"#,
        )
        .unwrap();
        let again: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(again["viewer"]["customGroupsByTeam"]["a:one"], groups);
    }

    use super::*;

    #[test]
    fn cp3_unchanged_load_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, "41", r#"{"viewer":{"settings":{"theme":"dark"}}}"#).unwrap();
        let writes = std::rc::Rc::new(std::cell::Cell::new(0));
        let counted = writes.clone();
        with_write_hook(
            move || counted.set(counted.get() + 1),
            || {
                load(&path, Some("41")).unwrap();
                load(&path, Some("42")).unwrap();
                load(&path, None).unwrap();
            },
        );
        assert_eq!(writes.get(), 0, "unchanged loads must not enter the writer");
    }

    #[test]
    fn legacy_cli_chrome_stays_with_its_original_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"org":{"activeOrgSlug":"a","activeTeamName":"one"},"viewer":{"schemaVersion":14,"layoutMode":{"a:one":"console"},"terminalVisible":{"a:one":true},"workspacePanelWidth":{"a:one":300}}}"#).unwrap();
        let first: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(first["viewer"]["layoutMode"]["a:one"], "console");
        let other: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        assert!(other["viewer"].get("layoutMode").is_none());
        assert!(other["viewer"].get("terminalVisible").is_none());
        assert!(other["viewer"].get("workspacePanelWidth").is_none());
        let again: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(again["viewer"]["workspacePanelWidth"]["a:one"], 300);
    }
    #[test]
    fn profiles_restore_independently_and_global_settings_survive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"org":{"activeOrgSlug":"A"},"viewer":{"openTabs":{"A":["secret"]},"settings":{"theme":"dark"}},"shortcuts":{"a":1}}"#).unwrap();
        let a: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(a["org"]["activeOrgSlug"], "A");
        let b: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        assert!(b.get("org").is_none());
        assert!(b["viewer"].get("openTabs").is_none());
        assert_eq!(b["viewer"]["settings"]["theme"], "dark");
        save(
            &path,
            "42",
            r#"{"org":{"activeOrgSlug":"B"},"viewer":{"settings":{"theme":"light"}}}"#,
        )
        .unwrap();
        let a: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(a["org"]["activeOrgSlug"], "A");
        assert_eq!(a["viewer"]["settings"]["theme"], "light");
        assert!(a.get(STORAGE).is_none());
        assert_eq!(a["shortcuts"]["a"], 1);
    }
    #[test]
    fn the_agent_context_pivot_is_scoped_to_its_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"viewer":{"settings":{"theme":"dark"}}}"#).unwrap();
        load(&path, Some("41")).unwrap();
        save(
            &path,
            "41",
            r#"{"viewer":{"selectedAgentContext":{"acme:default":{"agentKey":"acme/default/barb","repoSlug":"github.com/acme/codesense"}}}}"#,
        )
        .unwrap();
        let a: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(
            a["viewer"]["selectedAgentContext"]["acme:default"]["agentKey"],
            "acme/default/barb"
        );
        let b: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        assert!(
            b["viewer"].get("selectedAgentContext").is_none(),
            "another profile never sees this profile's pivot"
        );
        // Neither profile's write leaks it into the global viewer section on disk.
        let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(disk["viewer"].get("selectedAgentContext").is_none());
        assert!(!VIEWER.contains(&"selectedSourceRepo") && !VIEWER.contains(&"selectedContextId"));
    }
    #[test]
    fn the_cli_chrome_and_focus_are_scoped_to_their_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"viewer":{"settings":{"theme":"dark"}}}"#).unwrap();
        load(&path, Some("41")).unwrap();
        save(
            &path,
            "41",
            r#"{"viewer":{"settings":{"theme":"dark"},"cliChrome":{"visible":true,"layoutMode":"console","lastTerminalMode":"console","splitRatio":0.4,"expanded":true,"agentsDockCollapsed":true,"agentsDockWidth":280},"activeWorkspace":{"acme/widgets:ops":"ws-2"},"foregroundLayout":{"acme/widgets:ops":{"preset":"columns"}}}}"#,
        )
        .unwrap();
        let a: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(a["viewer"]["cliChrome"]["layoutMode"], "console");
        assert_eq!(a["viewer"]["cliChrome"]["expanded"], true);
        assert_eq!(a["viewer"]["cliChrome"]["agentsDockWidth"], 280);
        assert_eq!(a["viewer"]["activeWorkspace"]["acme/widgets:ops"], "ws-2");
        assert_eq!(
            a["viewer"]["foregroundLayout"]["acme/widgets:ops"]["preset"],
            "columns"
        );
        let b: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        for key in ["cliChrome", "activeWorkspace", "foregroundLayout"] {
            assert!(
                b["viewer"].get(key).is_none(),
                "another profile never sees {key}"
            );
        }
        let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(disk["viewer"].get("cliChrome").is_none());
        assert!(disk["viewer"].get("activeWorkspace").is_none());
        assert_eq!(disk["viewer"]["settings"]["theme"], "dark");
    }
    #[test]
    fn remote_tab_focus_is_scoped_to_its_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let key = "acme/widgets:ops|ws-2";
        let saved = serde_json::json!({"viewer": {"terminalViewerFocus": {key: "remote-second"}}});
        save(&path, "41", &saved.to_string()).unwrap();
        let other: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        assert!(other["viewer"].get("terminalViewerFocus").is_none());
        save(&path, "42", r#"{"viewer":{"terminalViewerFocus":{}}}"#).unwrap();
        let restored: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(
            restored["viewer"]["terminalViewerFocus"][key],
            "remote-second"
        );
        let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(disk["viewer"].get("terminalViewerFocus").is_none());
    }
    #[test]
    fn global_grids_upgrade_into_an_existing_profile_map_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let saved = serde_json::json!({
            STORAGE: {"41": {"viewer": {"workspaces": {"a:one": []}}}, "42": {"viewer": {"terminalViewerGrid": {"own": {"preset": "columns"}}}}},
            "viewer": {"terminalViewerGrid": {"a:one|w": {"preset": "rows"}}, "terminalTabGridSizes": {"a:one|w": {"sizes": [30,70]}}, "settings": {"theme": "dark"}}
        });
        std::fs::write(&path, saved.to_string()).unwrap();
        // The active/source principal is prepared before a profile switch.
        preserve_legacy(&path, Some("41")).unwrap();
        let other: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        assert_eq!(
            other["viewer"]["terminalViewerGrid"],
            saved[STORAGE]["42"]["viewer"]["terminalViewerGrid"]
        );
        assert!(other["viewer"].get("terminalTabGridSizes").is_none());
        let original: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        for field in ["terminalViewerGrid", "terminalTabGridSizes"] {
            assert_eq!(original["viewer"][field], saved["viewer"][field]);
        }
        let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(disk["viewer"].get("terminalViewerGrid").is_none());
        assert!(disk["viewer"].get("terminalTabGridSizes").is_none());
        assert_eq!(disk["viewer"]["settings"]["theme"], "dark");
    }
    #[test]
    fn eligible_legacy_workspace_is_attributed_before_global_grids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let saved = serde_json::json!({
            STORAGE: {"42": {"org": {"activeOrgSlug": "other"}}},
            LEGACY: {"org": {"activeOrgSlug": "original"}, "viewer": {"workspaces": {"a:one": [{"id": "w"}]}, "activeWorkspace": {"a:one": "w"}}},
            "viewer": {"terminalViewerGrid": {"a:one|w": {"preset": "rows"}}, "terminalTabGridSizes": {"a:one|w": {"sizes": [30,70]}}, "settings": {"theme": "dark"}}
        });
        std::fs::write(&path, saved.to_string()).unwrap();
        let original: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(original["org"]["activeOrgSlug"], "original");
        assert_eq!(
            original["viewer"]["workspaces"],
            saved[LEGACY]["viewer"]["workspaces"]
        );
        assert_eq!(
            original["viewer"]["activeWorkspace"],
            saved[LEGACY]["viewer"]["activeWorkspace"]
        );
        for field in ["terminalViewerGrid", "terminalTabGridSizes"] {
            assert_eq!(original["viewer"][field], saved["viewer"][field]);
        }
        let disk = read(&path).unwrap();
        assert!(disk.get(LEGACY).is_none());
        assert_eq!(disk[STORAGE]["42"], saved[STORAGE]["42"]);
    }
    #[test]
    fn global_grid_migration_keeps_the_destination_grid_and_fills_missing_dividers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let saved = serde_json::json!({
            STORAGE: {"41": {"viewer": {"terminalViewerGrid": {"own": {"preset": "columns"}}}}},
            "viewer": {"terminalViewerGrid": {"conflict": {"preset": "rows"}}, "terminalTabGridSizes": {"own": {"sizes": [30,70]}}}
        });
        std::fs::write(&path, saved.to_string()).unwrap();
        let original: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(
            original["viewer"]["terminalViewerGrid"],
            saved[STORAGE]["41"]["viewer"]["terminalViewerGrid"]
        );
        assert_eq!(
            original["viewer"]["terminalTabGridSizes"],
            saved["viewer"]["terminalTabGridSizes"]
        );
        let disk = read(&path).unwrap();
        assert!(disk["viewer"].get("terminalViewerGrid").is_none());
        assert!(disk["viewer"].get("terminalTabGridSizes").is_none());
        let other: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        assert!(other["viewer"].get("terminalViewerGrid").is_none());
        assert!(other["viewer"].get("terminalTabGridSizes").is_none());
    }
    #[test]
    fn remote_tab_grid_and_dividers_are_scoped_to_their_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let key = "acme/widgets:ops|ws-2";
        let saved = serde_json::json!({"viewer": {
            "terminalViewerGrid": {key: {"paned": [{"session": "remote", "member": "box/remote"}], "preset": "rows", "collapsed": false}},
            "terminalTabGridSizes": {key: {"preset": "rows", "root": {"kind": "leaf", "workspaceId": "remote"}}}
        }});
        save(&path, "41", &saved.to_string()).unwrap();
        let other: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        for field in ["terminalViewerGrid", "terminalTabGridSizes"] {
            assert!(
                other["viewer"].get(field).is_none(),
                "{field} leaked into another profile"
            );
        }
        save(
            &path,
            "42",
            r#"{"viewer":{"terminalViewerGrid":{},"terminalTabGridSizes":{}}}"#,
        )
        .unwrap();
        let restored: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        for field in ["terminalViewerGrid", "terminalTabGridSizes"] {
            assert_eq!(restored["viewer"][field], saved["viewer"][field]);
        }
        let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for field in ["terminalViewerGrid", "terminalTabGridSizes"] {
            assert!(disk["viewer"].get(field).is_none());
        }
    }
    #[test]
    fn dashboard_prefs_are_scoped_to_their_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"viewer":{"settings":{"theme":"dark"}}}"#).unwrap();
        load(&path, Some("41")).unwrap();
        save(
            &path,
            "41",
            r#"{"dashboardPrefs":{"pane:/ctx/acme/default/panes/pipeline.html":{"data":{"details":"full"},"at":1}}}"#,
        )
        .unwrap();
        let a: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(
            a["dashboardPrefs"]["pane:/ctx/acme/default/panes/pipeline.html"]["data"]["details"],
            "full"
        );
        let b: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        assert!(
            b.get("dashboardPrefs").is_none(),
            "another profile never sees this profile's dashboard prefs"
        );
        // Not left at the top level of the file, where every profile would read it.
        let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(disk.get("dashboardPrefs").is_none());
    }
    #[test]
    fn workspace_order_by_team_is_scoped_to_its_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"viewer":{"settings":{"theme":"dark"}}}"#).unwrap();
        load(&path, Some("41")).unwrap();
        save(
            &path,
            "41",
            r#"{"viewer":{"settings":{"theme":"dark"},"workspaceOrderByTeam":{"acme/widgets:core":["ws-2","ws-1"]}}}"#,
        )
        .unwrap();
        let a: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(
            a["viewer"]["workspaceOrderByTeam"]["acme/widgets:core"],
            serde_json::json!(["ws-2", "ws-1"])
        );
        let b: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        assert!(
            b["viewer"].get("workspaceOrderByTeam").is_none(),
            "another profile never sees this profile's workspace order"
        );
        // Not left in the top-level viewer section, where every profile would read it.
        let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(disk["viewer"].get("workspaceOrderByTeam").is_none());
        assert_eq!(disk["viewer"]["settings"]["theme"], "dark");
    }
    #[test]
    fn workspace_order_and_its_tray_base_travel_together_per_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"viewer":{"settings":{"theme":"dark"}}}"#).unwrap();
        load(&path, Some("41")).unwrap();
        save(
            &path,
            "41",
            r#"{"viewer":{"workspaceOrderByTeam":{"acme/widgets:core":["ws-2","ws-1"]},"trayBaseOrderByTeam":{"acme/widgets:core":["ws-1","ws-2"]},"agentsPaneFolds":{"acme/widgets:core":true},"agentsWorkspaceFilter":{"teamKey":"acme/widgets:core","workspaceId":"ws-1"}}}"#,
        )
        .unwrap();
        let a: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(
            a["viewer"]["workspaceOrderByTeam"]["acme/widgets:core"],
            serde_json::json!(["ws-2", "ws-1"])
        );
        assert_eq!(
            a["viewer"]["trayBaseOrderByTeam"]["acme/widgets:core"],
            serde_json::json!(["ws-1", "ws-2"])
        );
        assert_eq!(a["viewer"]["agentsPaneFolds"]["acme/widgets:core"], true);
        assert_eq!(a["viewer"]["agentsWorkspaceFilter"]["workspaceId"], "ws-1");
        let b: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        for key in [
            "workspaceOrderByTeam",
            "trayBaseOrderByTeam",
            "agentsPaneFolds",
            "agentsWorkspaceFilter",
        ] {
            assert!(
                b["viewer"].get(key).is_none(),
                "another profile never sees {key}"
            );
        }
        let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(disk["viewer"].get("workspaceOrderByTeam").is_none());
        assert!(disk["viewer"].get("trayBaseOrderByTeam").is_none());
        assert!(!VIEWER.contains(&"agentTreeCollapsed"));
    }
    #[test]
    fn the_agents_pane_folds_hidden_teams_and_filter_are_scoped_to_their_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"viewer":{"settings":{"theme":"dark"}}}"#).unwrap();
        load(&path, Some("41")).unwrap();
        save(
            &path,
            "41",
            r#"{"viewer":{"settings":{"theme":"dark"},"agentsPaneFolds":{"acme/widgets:core":true},"agentsPaneHiddenTeams":["acme/widgets:ops"],"agentsWorkspaceFilter":{"teamKey":"acme/widgets:core","workspaceId":"ws-1"}}}"#,
        )
        .unwrap();
        let a: Value = serde_json::from_str(&load(&path, Some("41")).unwrap()).unwrap();
        assert_eq!(a["viewer"]["agentsPaneFolds"]["acme/widgets:core"], true);
        assert_eq!(
            a["viewer"]["agentsPaneHiddenTeams"],
            serde_json::json!(["acme/widgets:ops"])
        );
        assert_eq!(a["viewer"]["agentsWorkspaceFilter"]["workspaceId"], "ws-1");
        let b: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        for key in [
            "agentsPaneFolds",
            "agentsPaneHiddenTeams",
            "agentsWorkspaceFilter",
        ] {
            assert!(
                b["viewer"].get(key).is_none(),
                "another profile never sees {key}"
            );
        }
        // None of them is left in the global viewer section, where every profile would read it.
        let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for key in [
            "agentsPaneFolds",
            "agentsPaneHiddenTeams",
            "agentsWorkspaceFilter",
        ] {
            assert!(
                disk["viewer"].get(key).is_none(),
                "{key} stays in its profile"
            );
        }
        assert_eq!(disk["viewer"]["settings"]["theme"], "dark");
    }
    #[test]
    fn ambiguous_legacy_snapshot_is_preserved_without_claiming_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"org":{"activeOrgSlug":"unattributed"}}"#).unwrap();
        let chooser: Value = serde_json::from_str(&load(&path, None).unwrap()).unwrap();
        assert!(chooser.get("org").is_none());
        preserve_legacy(&path, None).unwrap();
        let selected: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
        assert!(selected.get("org").is_none());
        assert_eq!(
            read(&path).unwrap()[LEGACY]["org"]["activeOrgSlug"],
            "unattributed"
        );
    }
    #[test]
    fn no_principal_archives_global_grids_without_importing_them() {
        for preserve_first in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state.json");
            let saved = serde_json::json!({
                STORAGE: {"41": {"org": {"activeOrgSlug": "existing"}}},
                LEGACY: {"org": {"activeOrgSlug": "unassigned"}},
                "viewer": {
                    "terminalViewerGrid": {"a:one|w": {"preset": "rows"}},
                    "terminalTabGridSizes": {"a:one|w": {"sizes": [30, 70]}},
                    "settings": {"theme": "dark"}
                }
            });
            std::fs::write(&path, saved.to_string()).unwrap();
            if preserve_first {
                preserve_legacy(&path, None).unwrap();
            }
            let chooser: Value = serde_json::from_str(&load(&path, None).unwrap()).unwrap();
            for field in ["terminalViewerGrid", "terminalTabGridSizes"] {
                assert!(chooser["viewer"].get(field).is_none());
                assert_eq!(
                    read(&path).unwrap()[LEGACY]["viewer"][field],
                    saved["viewer"][field]
                );
            }
            preserve_legacy(&path, None).unwrap();
            let selected: Value = serde_json::from_str(&load(&path, Some("42")).unwrap()).unwrap();
            let disk = read(&path).unwrap();
            for field in ["terminalViewerGrid", "terminalTabGridSizes"] {
                assert!(selected["viewer"].get(field).is_none());
                assert!(disk["viewer"].get(field).is_none());
                assert_eq!(disk[LEGACY]["viewer"][field], saved["viewer"][field]);
                assert!(disk[STORAGE].get("42").is_none());
            }
            assert_eq!(disk[STORAGE]["41"], saved[STORAGE]["41"]);
            assert_eq!(disk[LEGACY]["org"], saved[LEGACY]["org"]);
            assert_eq!(selected["viewer"]["settings"]["theme"], "dark");
        }
    }
    #[test]
    fn malformed_saved_profile_map_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let bytes = format!(r#"{{"{STORAGE}":[]}}"#);
        std::fs::write(&path, &bytes).unwrap();
        assert!(save(&path, "41", "{}").is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), bytes);
    }
    #[test]
    fn malformed_profile_entries_reject_load_save_and_legacy_attribution_without_byte_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        for map in [
            serde_json::json!({"41":null}),
            serde_json::json!({"42":[]}),
            serde_json::json!({"41":5}),
            serde_json::json!({"../41":{}}),
            serde_json::json!({"041":{}}),
        ] {
            let bytes =
                serde_json::to_vec(&serde_json::json!({STORAGE:map,"shortcuts":{"keep":true}}))
                    .unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(load(&path, Some("41")).is_err());
            assert!(save(&path, "41", "{}").is_err());
            assert!(preserve_legacy(&path, Some("41")).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }
}
