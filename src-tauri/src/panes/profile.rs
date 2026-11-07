// SPDX-License-Identifier: MIT
use super::DashboardRef;
use crate::{content_engine::atomic_io, context_repo::scratch_space, domain::Principal};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SlotChoice {
    Many(Vec<DashboardRef>),
    One(DashboardRef),
    Native(Native),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Native {
    Native,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub right_pane_surfaces: BTreeMap<String, SlotChoice>,
    pub id: String,
    pub name: String,
    pub posture: String,
    #[serde(default)]
    pub slots: BTreeMap<String, SlotChoice>,
    pub cli_layout: String,
    #[serde(default)]
    pub chrome: BTreeMap<String, serde_json::Value>,
}
#[cfg(test)]
pub fn read(home: &Path, principal: &Principal, org: &str) -> Result<Option<Profile>, String> {
    let path = scratch_space::org_space_at(home, principal, org)?
        .config_dir()
        .join("profile.json");
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
pub(crate) fn empty() -> serde_json::Value {
    serde_json::json!({"id":"default","name":"Default","posture":"","slots":{},"cliLayout":"default","chrome":{}})
}

pub(crate) fn merge_patch(
    value: &mut serde_json::Value,
    patch: serde_json::Value,
) -> Result<(), String> {
    let fields = patch.as_object().ok_or("profile patch must be an object")?;
    for (field, next) in fields {
        if matches!(field.as_str(), "slots" | "rightPaneSurfaces" | "chrome") {
            let entries = next
                .as_object()
                .ok_or("profile collection must be an object")?;
            if !value[field].is_object() {
                value[field] = serde_json::json!({});
            }
            for (key, next) in entries {
                value[field][key] = next.clone();
            }
        } else {
            value[field] = next.clone();
        }
    }
    Ok(())
}

pub(crate) fn patch(
    home: &Path,
    principal: &Principal,
    org: &str,
    patch: serde_json::Value,
) -> Result<Profile, String> {
    let (space, _) = scratch_space::ensure_org_space(home, principal, org)?;
    let path = space.config_dir().join("profile.json");
    let mut value = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => empty(),
        Err(e) => return Err(e.to_string()),
    };
    merge_patch(&mut value, patch)?;
    let profile: Profile = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    validate(&profile)?;
    atomic_io::write_tmp_fsync_rename(
        &path,
        &serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(profile)
}

fn validate(profile: &Profile) -> Result<(), String> {
    for (slot, choice) in &profile.slots {
        if !crate::verbs::capabilities::SLOTS.contains(&slot.as_str()) {
            return Err(format!("unknown slot {slot:?}"));
        }
        if slot == "right-pane" && matches!(choice, SlotChoice::One(_)) {
            return Err("right-pane stores a list of dashboards".into());
        }
        if matches!(slot.as_str(), "dock" | "file-nav") && matches!(choice, SlotChoice::Many(_)) {
            return Err(format!("{slot} stores one dashboard"));
        }
    }
    Ok(())
}
#[cfg(test)]
pub fn write(
    home: &Path,
    principal: &Principal,
    org: &str,
    profile: &Profile,
) -> Result<(), String> {
    validate(profile)?;
    let (space, _) = scratch_space::ensure_org_space(home, principal, org)?;
    atomic_io::write_tmp_fsync_rename(
        &space.config_dir().join("profile.json"),
        &serde_json::to_vec_pretty(profile).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panes::{DashboardRef, DashboardScope};
    use serde_json::json;

    #[test]
    fn file_nav_accepts_one_ref_or_native_and_rejects_a_list() {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(42u64);
        for choice in [
            json!("native"),
            json!({"scope":"built-in","org":"acme/api","id":"tree"}),
        ] {
            let profile: Profile = serde_json::from_value(json!({"id":"default","name":"Default","posture":"","slots":{"file-nav":choice},"cliLayout":"default"})).unwrap();
            write(home.path(), &principal, "acme/api", &profile).unwrap();
            assert_eq!(
                read(home.path(), &principal, "acme/api").unwrap().unwrap(),
                profile
            );
        }
        let profile: Profile = serde_json::from_value(json!({"id":"default","name":"Default","posture":"","slots":{"file-nav":[]},"cliLayout":"default"})).unwrap();
        assert_eq!(
            write(home.path(), &principal, "acme/api", &profile).unwrap_err(),
            "file-nav stores one dashboard"
        );
    }

    #[test]
    fn surface_choices_default_empty_and_round_trip_without_materializing_other_surfaces() {
        let home = tempfile::tempdir().unwrap();
        let principal = crate::domain::Principal::from(42u64);
        let mut profile: Profile = serde_json::from_value(json!({"id":"default","name":"Default","posture":"","slots":{"right-pane":"native"},"cliLayout":"default"})).unwrap();
        assert!(profile.right_pane_surfaces.is_empty());
        profile
            .right_pane_surfaces
            .insert("notes".into(), SlotChoice::Native(Native::Native));
        write(home.path(), &principal, "acme/repo", &profile).unwrap();
        let restored = read(home.path(), &principal, "acme/repo").unwrap().unwrap();
        assert_eq!(restored, profile);
        assert!(!restored.right_pane_surfaces.contains_key("inbox"));
        assert_eq!(
            restored.slots["right-pane"],
            SlotChoice::Native(Native::Native)
        );
    }

    #[test]
    fn profile_round_trips_refs_and_native_defaults_in_private_config() {
        let home = tempfile::tempdir().unwrap();
        let principal = crate::domain::Principal::from(42u64);
        assert!(read(home.path(), &principal, "acme/repo")
            .unwrap()
            .is_none());
        let profile: Profile = serde_json::from_value(json!({"id":"default","name":"Default","posture":"calm","slots":{"right-pane":[{"scope":"private","org":"acme/repo","id":"inbox","installedFrom":{"scope":"team","org":"acme/repo","team":"dev","id":"inbox"}}],"dock":"native"},"cliLayout":"default","chrome":{}})).unwrap();
        write(home.path(), &principal, "acme/repo", &profile).unwrap();
        assert_eq!(
            read(home.path(), &principal, "acme/repo").unwrap(),
            Some(profile)
        );
        let path =
            crate::context_repo::scratch_space::org_space_at(home.path(), &principal, "acme/repo")
                .unwrap()
                .config_dir()
                .join("profile.json");
        assert!(path.is_file());
    }

    #[test]
    fn built_in_dashboard_refs_round_trip_through_the_profile_with_stable_keys() {
        let home = tempfile::tempdir().unwrap();
        let principal = crate::domain::Principal::from(42u64);
        let profile: Profile = serde_json::from_value(json!({"id":"default","name":"Default","posture":"calm","slots":{"right-pane":[{"scope":"built-in","org":"acme/repo","id":"board"}]},"cliLayout":"default","chrome":{}})).unwrap();
        write(home.path(), &principal, "acme/repo", &profile).unwrap();
        let restored = read(home.path(), &principal, "acme/repo").unwrap().unwrap();
        assert_eq!(restored, profile);
        let SlotChoice::Many(refs) = &restored.slots["right-pane"] else {
            panic!("right-pane list")
        };
        assert_eq!(refs[0].scope, DashboardScope::BuiltIn);
        assert_eq!(refs[0].key(), "dashboard:built-in:acme/repo/board");
    }

    #[test]
    fn ref_keys_distinguish_scopes_and_ignore_copy_provenance() {
        let mut dashboard = DashboardRef {
            scope: DashboardScope::Team,
            org: "acme/repo".into(),
            team: Some("dev".into()),
            id: "board".into(),
            installed_from: None,
        };
        assert_eq!(dashboard.key(), "dashboard:team:acme/repo:dev/board");
        let source = dashboard.clone();
        dashboard.scope = DashboardScope::Private;
        dashboard.team = None;
        dashboard.installed_from = Some(Box::new(source));
        assert_eq!(dashboard.key(), "dashboard:private:acme/repo/board");
        dashboard.scope = DashboardScope::BuiltIn;
        assert_eq!(dashboard.key(), "dashboard:built-in:acme/repo/board");
    }
}
