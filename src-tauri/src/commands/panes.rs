// SPDX-License-Identifier: MIT
//! The app's view of custom right panes (`<space>/panes/<id>.html`).

use crate::domain::Principal;
use crate::panes::PaneListing;
use tauri::Manager;

#[cfg(test)]
mod caller_scope_tests;

/// The panes `org/team` sees: its chain's team panes, nearest first, then the
/// private space's, with the content root their reads resolve against.
#[tauri::command]
pub async fn list_panes(
    app: tauri::AppHandle,
    org: String,
    team: String,
    source_view: Option<String>,
    source_seq: Option<u64>,
    selected_refs: Option<Vec<crate::panes::DashboardRef>>,
    refresh_ref: Option<crate::panes::DashboardRef>,
) -> Result<serde_json::Value, String> {
    let (home, user_id) = crate::commands::route_to_context::resolve_ambient(&app).await?;
    crate::commands::await_blocking("list_panes", move || {
        let principal = Principal::from(user_id);
        if let Some(owner) = source_view {
            crate::live_hub::pane_sources::open(
                &app,
                crate::live_hub::pane_sources::ViewRequest {
                    home: &home,
                    principal: &principal,
                    owner: &owner,
                    sequence: source_seq.unwrap_or(0),
                    org: &org,
                    team: &team,
                    selected: selected_refs.unwrap_or_default(),
                    retry: refresh_ref,
                },
            )
        } else {
            serde_json::to_value(crate::panes::list_panes(&home, &principal, &org, &team)?)
                .map_err(|e| e.to_string())
        }
    })
    .await
}

/// Bundled recovery descriptors and the requested team's read context, without space discovery.
#[tauri::command]
pub async fn list_builtin_panes(
    app: tauri::AppHandle,
    org: String,
    team: String,
) -> Result<PaneListing, String> {
    let (home, user_id) = crate::commands::route_to_context::resolve_ambient(&app).await?;
    crate::commands::await_blocking("list_builtin_panes", move || {
        crate::panes::list_builtin_panes(&home, &Principal::from(user_id), &org, &team)
    })
    .await
}

#[tauri::command]
pub async fn write_dashboard_profile(
    app: tauri::AppHandle,
    org: String,
    patch: serde_json::Value,
    expected_principal: Option<String>,
) -> Result<crate::panes::profile::Profile, String> {
    let (home, user_id) = crate::commands::route_to_context::resolve_ambient(&app).await?;
    let principal = Principal::from(user_id);
    if expected_principal.as_deref() != Some(principal.as_str()) {
        return Err("unavailable: profile changed".into());
    }
    let policy = crate::verbs::commands::read_policy(app.clone(), &user_id.to_string()).await?;
    crate::commands::await_blocking("write_dashboard_profile", move || {
        app.state::<crate::commands::local_profile::LocalProfileState>()
            .with_expected(expected_principal.as_deref(), || {
                write_profile_patch(
                    &app,
                    &home,
                    &principal,
                    &org,
                    patch,
                    policy.effective_switches().private_install,
                )
            })
    })
    .await
}

pub(crate) fn write_profile_patch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    home: &std::path::Path,
    principal: &Principal,
    org: &str,
    mut patch: serde_json::Value,
    private_install: bool,
) -> Result<crate::panes::profile::Profile, String> {
    static WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _write = WRITES.lock().map_err(|e| e.to_string())?;
    let mut choices = crate::panes::profile::empty();
    crate::panes::profile::merge_patch(&mut choices, patch.clone())?;
    let mut choices = serde_json::from_value(choices).map_err(|e| e.to_string())?;
    install_profile_choices_for(
        home,
        principal,
        &mut choices,
        private_install,
        &crate::context_repo::caller_scope::CallerScope::Human,
    )?;
    if patch.get("slots").is_some() {
        patch["slots"] = serde_json::to_value(&choices.slots).unwrap();
    }
    if patch.get("rightPaneSurfaces").is_some() {
        patch["rightPaneSurfaces"] = serde_json::to_value(&choices.right_pane_surfaces).unwrap();
    }
    let saved = crate::panes::profile::patch(home, principal, org, patch)?;
    crate::live_hub::configuration::value_changed(
        app,
        &crate::live_hub::configuration::profile_path(home, principal, org)?,
        serde_json::to_value(&saved).unwrap(),
    )?;
    Ok(saved)
}

#[cfg(test)]
fn install_profile_choices(
    home: &std::path::Path,
    principal: &Principal,
    profile: &mut crate::panes::profile::Profile,
    private_install: bool,
) -> Result<(), String> {
    install_profile_choices_for(
        home,
        principal,
        profile,
        private_install,
        &Default::default(),
    )
}

fn install_profile_choices_for(
    home: &std::path::Path,
    principal: &Principal,
    profile: &mut crate::panes::profile::Profile,
    private_install: bool,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<(), String> {
    // Architecture: both profile collections select through the same existing installation policy.
    for (slot, choice) in profile
        .slots
        .iter_mut()
        .map(|(slot, choice)| (slot.as_str(), choice))
        .chain(
            profile
                .right_pane_surfaces
                .values_mut()
                .map(|choice| ("right-pane", choice)),
        )
    {
        let refs = match choice {
            crate::panes::profile::SlotChoice::Many(refs) => refs.as_mut_slice(),
            crate::panes::profile::SlotChoice::One(dashboard) => std::slice::from_mut(dashboard),
            crate::panes::profile::SlotChoice::Native(_) => continue,
        };
        for dashboard in refs {
            if dashboard.scope != crate::panes::DashboardScope::Team {
                continue;
            }
            let team = dashboard
                .team
                .as_deref()
                .ok_or("Team dashboard has no team")?;
            let listing = crate::panes::list_panes(home, principal, &dashboard.org, team)?;
            let pane = listing
                .panes
                .iter()
                .find(|p| {
                    crate::panes::DashboardRef::key(&p.dashboard_ref)
                        == crate::panes::DashboardRef::key(dashboard)
                })
                .ok_or(crate::context_repo::caller_scope::MISSING)?;
            *dashboard = crate::panes::install::choose_for(
                home,
                principal,
                pane,
                pane.declaration.private_reach(slot),
                private_install,
                scope,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panes::{
        profile::{Profile, SlotChoice},
        DashboardScope,
    };
    use serde_json::json;

    #[test]
    fn surface_and_slot_choices_share_private_installation_and_open_policy() {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(42u64);
        let org = "acme/repo";
        let root = crate::context_repo::path_to_org_root(home.path(), &principal, org).unwrap();
        let dir = root.join("dev/panes");
        std::fs::create_dir_all(&dir).unwrap();
        for id in ["notes", "inbox"] {
            std::fs::write(dir.join(format!("{id}.html")), format!(r#"<title>{id}</title><meta name="codesense:slots" content="right-pane"><meta name="codesense:surface" content="{id}">"#)).unwrap();
        }
        let notes = json!({"scope":"team","org":org,"team":"dev","id":"notes"});
        let inbox = json!({"scope":"team","org":org,"team":"dev","id":"inbox"});
        let original: Profile = serde_json::from_value(json!({"id":"default","name":"Default","posture":"","cliLayout":"default","slots":{"right-pane":[notes.clone()]},"rightPaneSurfaces":{"notes":notes,"inbox":inbox}})).unwrap();
        let mut open = original.clone();
        install_profile_choices(home.path(), &principal, &mut open, false).unwrap();
        assert_eq!(open, original);
        let mut installed = original.clone();
        install_profile_choices(home.path(), &principal, &mut installed, true).unwrap();
        let SlotChoice::Many(slot_refs) = &installed.slots["right-pane"] else {
            panic!("slot refs")
        };
        let SlotChoice::One(notes_ref) = &installed.right_pane_surfaces["notes"] else {
            panic!("notes ref")
        };
        assert_eq!(&slot_refs[0], notes_ref);
        for choice in installed.right_pane_surfaces.values() {
            let SlotChoice::One(reference) = choice else {
                panic!("surface ref")
            };
            assert_eq!(reference.scope, DashboardScope::Private);
            assert_eq!(
                reference.installed_from.as_ref().unwrap().scope,
                DashboardScope::Team
            );
        }
        crate::panes::profile::write(home.path(), &principal, org, &installed).unwrap();
        assert_eq!(
            crate::panes::profile::read(home.path(), &principal, org).unwrap(),
            Some(installed)
        );
    }
}
