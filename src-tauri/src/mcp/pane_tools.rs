// SPDX-License-Identifier: MIT
//! The custom right-pane MCP tools: `pane_list`, `pane_write`, `pane_open`.
//!
//! The `#[tool]` methods in [`super::handler`] resolve who is calling and where
//! a pane goes; the bodies here are thin adapters over [`crate::panes`]. A pane
//! is a plain file: writing it is creating or replacing it, deleting the file
//! deletes it, and sharing a team pane is the agent's own git push.

use std::path::Path;

use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::tool_envelope::ToolEnvelope;
use crate::domain::Principal;
use crate::panes::{self, PaneListing, PaneScope, PaneTarget};

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PaneListParams {
    /// The org (`owner/repo`); pass with `team`, or omit both for the active team.
    #[serde(default)]
    pub org: Option<String>,
    /// The team whose chain to list; pass with `org`, or omit both.
    #[serde(default)]
    pub team: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PaneWriteParams {
    /// `team` (default) or `personal` (your private space for `org`, which is
    /// then required).
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    /// For scope `team`: the team, with `org`; omit both for the active team.
    #[serde(default)]
    pub team: Option<String>,
    /// The pane id: its file name without `.html`, `[a-z0-9][a-z0-9-]*`.
    pub id: String,
    /// The whole HTML document.
    pub html: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PaneOpenParams {
    /// `team` (default) or `personal`.
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
    /// The pane id, as pane_list returns it.
    pub id: String,
    /// Opaque page intent, a JSON object of at most 4096 UTF-8 bytes.
    #[serde(default)]
    pub intent: Option<serde_json::Map<String, serde_json::Value>>,
}

/// What `pane_write` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneWritten {
    pub id: String,
    pub scope: PaneScope,
    pub org: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub path: String,
    pub panel_id: String,
    /// False when it replaced an existing pane of that id.
    pub created: bool,
}

/// What `pane_open` reports, and the payload of the open event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneOpened {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<serde_json::Map<String, serde_json::Value>>,
    pub panel_id: String,
    pub path: String,
    /// Whether the app placed it: only a pane the active team shows can be.
    pub placed: bool,
    /// Why it was not placed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `pane_list`: the panes `org/team` sees.
#[cfg(test)]
pub(crate) fn list(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<ToolEnvelope<PaneListing>, String> {
    list_for(home, principal, org, team, &Default::default())
}

pub(crate) fn list_for(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<ToolEnvelope<PaneListing>, String> {
    let mut listing = panes::list_panes(home, principal, org, team)?;
    listing
        .panes
        .retain(|pane| pane.dashboard_ref.scope != panes::DashboardScope::BuiltIn);
    listing
        .panes
        .retain(|pane| scope.allows(Path::new(&pane.path)));
    if let crate::context_repo::caller_scope::CallerScope::Team(view) = scope {
        listing.panes = listing
            .panes
            .into_iter()
            .filter_map(|pane| {
                if pane.scope == PaneScope::Team {
                    panes::read_pane_for_view(
                        view,
                        Path::new(&pane.path),
                        pane.scope,
                        org,
                        pane.team.as_deref(),
                    )
                } else {
                    Some(pane)
                }
            })
            .collect();
        listing.manifests = listing
            .manifests
            .into_iter()
            .filter_map(|mut layer| {
                let pane = listing
                    .panes
                    .iter()
                    .find(|p| p.scope == layer.scope && p.team == layer.team)?;
                layer.manifest = view
                    .pane_manifest_at(Path::new(&pane.panes_dir), layer.team.as_deref())?
                    .ok()?;
                Some(layer)
            })
            .collect();
        if view.org() == org {
            listing.panes.retain(|p| {
                p.scope != PaneScope::Private
                    || p.dashboard_ref.scope == crate::panes::DashboardScope::BuiltIn
            });
            listing.manifests.retain(|m| m.scope != PaneScope::Private);
            let private = panes::list_private_panes(home, principal, org, view.reader_team());
            listing.panes.extend(private.panes);
            listing.manifests.extend(private.manifests);
        }
        if view.org() == org && view.inherited.iter().any(|l| l.team() == team) {
            if let Some(root) = view.own_content().own_path() {
                listing.content_root = root.to_string_lossy().into();
            }
        }
    }
    if matches!(
        scope,
        crate::context_repo::caller_scope::CallerScope::Unknown
    ) {
        let private_manifest =
            crate::context_repo::scratch_space::org_space_at(home, principal, org)
                .map(|space| space.panes_dir().join(panes::manifest::MANIFEST_FILE));
        listing.manifests.retain(|layer| {
            layer.scope == PaneScope::Private
                && private_manifest
                    .as_ref()
                    .is_ok_and(|path| scope.allows(path))
        });
    }
    // Architecture: shadow bundled fallback only with admitted panes.
    for builtin in panes::builtin_panes(home, principal, org) {
        if !listing.panes.iter().any(|pane| pane.id == builtin.id) {
            listing.panes.push(builtin);
        }
    }
    Ok(ToolEnvelope::Ok(listing))
}

pub(crate) fn write_for(
    home: &Path,
    principal: &Principal,
    target: &PaneTarget,
    id: &str,
    html: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<ToolEnvelope<PaneWritten>, String> {
    let readable = matches!(scope, crate::context_repo::caller_scope::CallerScope::Human)
        || target
            .panes_dir(home, principal)
            .ok()
            .is_some_and(|dir| scope.allows(&dir.join(format!("{id}.html"))));
    crate::context_repo::caller_scope::readback(readable, write(home, principal, target, id, html))
}

/// `pane_write`: write `html` as pane `id` at `target`.
fn write(
    home: &Path,
    principal: &Principal,
    target: &PaneTarget,
    id: &str,
    html: &str,
) -> Result<ToolEnvelope<PaneWritten>, String> {
    if !panes::is_valid_pane_id(id) {
        return Ok(ToolEnvelope::Validation {
            message: format!(
                "id {id:?} must match [a-z0-9][a-z0-9-]* (it is the file name: {id}.html)"
            ),
        });
    }
    let dir = match target.panes_dir(home, principal) {
        Ok(dir) => dir,
        Err(message) => return Ok(ToolEnvelope::Validation { message }),
    };
    let created = !dir.join(format!("{id}.html")).exists();
    let path = panes::write_pane(&dir, id, html)?;
    Ok(ToolEnvelope::Ok(PaneWritten {
        id: id.to_string(),
        scope: target.scope(),
        org: target.org().to_string(),
        team: target.team().map(str::to_string),
        path: path.to_string_lossy().into_owned(),
        panel_id: panes::pane_panel_id(target.scope(), target.org(), target.team(), id),
        created,
    }))
}

/// `pane_open`: the panel to place for pane `id`. A team target resolves the id
/// through the team's chain, nearest first, exactly as pane_list lists it, so a
/// pane inherited from a parent team opens by its id; a private target names
/// the private space's pane.
#[cfg(test)]
pub(crate) fn open(
    home: &Path,
    principal: &Principal,
    target: &PaneTarget,
    id: &str,
    active: Option<(&str, &str)>,
    intent: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<ToolEnvelope<PaneOpened>, String> {
    open_for(
        home,
        principal,
        target,
        id,
        active,
        intent,
        &Default::default(),
    )
}

pub(crate) fn open_for(
    home: &Path,
    principal: &Principal,
    target: &PaneTarget,
    id: &str,
    active: Option<(&str, &str)>,
    intent: Option<serde_json::Map<String, serde_json::Value>>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<ToolEnvelope<PaneOpened>, String> {
    if !panes::is_valid_pane_id(id) {
        return Ok(ToolEnvelope::Validation {
            message: format!("id {id:?} must match [a-z0-9][a-z0-9-]*"),
        });
    }
    if intent
        .as_ref()
        .is_some_and(|i| serde_json::to_vec(i).map_or(true, |bytes| bytes.len() > 4096))
    {
        return Ok(ToolEnvelope::Validation {
            message: "intent must be a JSON object of at most 4096 bytes".into(),
        });
    }
    let found = match target {
        PaneTarget::Team { org, team } => match list_for(home, principal, org, team, scope)? {
            ToolEnvelope::Ok(listing) => listing,
            _ => {
                return Ok(ToolEnvelope::Validation {
                    message: crate::context_repo::caller_scope::MISSING.into(),
                })
            }
        }
        .panes
        .into_iter()
        .find(|p| p.scope == PaneScope::Team && p.id == id)
        .map(|p| (p.panel_id, p.path)),
        PaneTarget::Private { org } => {
            let team = active
                .filter(|(active_org, _)| *active_org == org)
                .map(|(_, team)| team)
                .unwrap_or(crate::context_repo::scratch_space::SCRATCH_TEAM);
            panes::list_private_panes(home, principal, org, team)
                .panes
                .into_iter()
                .find(|p| p.scope == PaneScope::Private && p.id == id)
                .map(|p| (p.panel_id, p.path))
        }
    }
    .filter(|(_, path)| scope.allows(Path::new(path)));
    // The app shows the active team's chain and its org's private panes; a pane
    // outside them cannot be placed, and the reply says so rather than claim it.
    let shown = |panel_id: &str| match active {
        Some((org, team)) => panes::list_panes(home, principal, org, team)
            .map(|listing| listing.panes.iter().any(|p| p.panel_id == panel_id))
            .unwrap_or(false),
        None => false,
    };
    Ok(match found {
        Some((panel_id, path)) => {
            let placed = shown(&panel_id);
            ToolEnvelope::Ok(PaneOpened {
                intent,
                reason: (!placed).then(|| {
                    "the active team does not show this pane; switch to its team (or a child team, \
                     or its org for a private pane) and call pane_open again"
                        .to_string()
                }),
                panel_id,
                path,
                placed,
            })
        }
        None if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) => {
            ToolEnvelope::Validation {
                message: crate::context_repo::caller_scope::MISSING.into(),
            }
        }
        None => ToolEnvelope::Validation {
            message: format!(
                "no pane {id:?} there; write it with pane_write first, or pick one from pane_list"
            ),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_repo::{path_to_org_root, path_to_team_panes_dir};

    const ORG: &str = "acme/api";

    fn team_target() -> PaneTarget {
        PaneTarget::Team {
            org: ORG.into(),
            team: "default".into(),
        }
    }

    fn setup() -> (tempfile::TempDir, Principal) {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(42u64);
        let org_root = path_to_org_root(home.path(), &principal, ORG).unwrap();
        std::fs::create_dir_all(org_root.join("default/content")).unwrap();
        (home, principal)
    }

    #[test]
    fn golden_example_writes_opens_and_lists_the_exact_team_and_private_file() {
        let (home, principal) = setup();
        let html = include_str!("../../../codesense-docs/getting-started/content/examples/dashboards/live-list-with-command.html");
        for target in [team_target(), PaneTarget::Private { org: ORG.into() }] {
            let ToolEnvelope::Ok(written) = write(home.path(), &principal, &target, "live-list-with-command", html).unwrap() else { panic!("write") };
            let ToolEnvelope::Ok(opened) = open(home.path(), &principal, &target, "live-list-with-command", Some((ORG, "default")), None).unwrap() else { panic!("open") };
            assert!(opened.placed);
            assert_eq!(opened.path, written.path);
            assert_eq!(std::fs::read_to_string(&opened.path).unwrap(), html);
            let ToolEnvelope::Ok(listing) = list(home.path(), &principal, ORG, "default").unwrap() else { panic!("list") };
            let row = listing.panes.iter().find(|row| row.path == opened.path).unwrap();
            assert_ne!(row.status, panes::PaneStatus::Rejected);
            assert_eq!(row.declaration.commands[0].id, "toggle-filter");
        }
    }

    #[test]
    fn write_creates_then_replaces_and_open_names_the_panel() {
        let (home, principal) = setup();
        let target = team_target();
        let ToolEnvelope::Ok(first) = write(
            home.path(),
            &principal,
            &target,
            "board",
            "<title>B</title>",
        )
        .unwrap() else {
            panic!("write ok")
        };
        assert!(first.created);
        assert_eq!(first.panel_id, "pane:acme/api:default/board");
        let ToolEnvelope::Ok(second) = write(
            home.path(),
            &principal,
            &target,
            "board",
            "<title>B2</title>",
        )
        .unwrap() else {
            panic!("write ok")
        };
        assert!(!second.created, "a plain overwrite");
        let org_root = path_to_org_root(home.path(), &principal, ORG).unwrap();
        assert_eq!(
            std::fs::read_to_string(
                path_to_team_panes_dir(&org_root, "default").join("board.html")
            )
            .unwrap(),
            "<title>B2</title>"
        );
        let ToolEnvelope::Ok(opened) = open(
            home.path(),
            &principal,
            &target,
            "board",
            Some((ORG, "default")),
            None,
        )
        .unwrap() else {
            panic!("open ok")
        };
        assert_eq!(opened.panel_id, "pane:acme/api:default/board");
        assert!(opened.placed);
        let ToolEnvelope::Ok(elsewhere) = open(
            home.path(),
            &principal,
            &target,
            "board",
            Some((ORG, "other")),
            None,
        )
        .unwrap() else {
            panic!("open ok")
        };
        assert!(
            !elsewhere.placed,
            "a team that does not show it places nothing"
        );
        assert!(elsewhere.reason.is_some());
    }

    #[test]
    fn open_resolves_an_inherited_pane_through_the_chain() {
        let (home, principal) = setup();
        let org_root = path_to_org_root(home.path(), &principal, ORG).unwrap();
        std::fs::create_dir_all(org_root.join("platform/content")).unwrap();
        std::fs::write(
            org_root.join("org.yml"),
            format!("slug: {ORG}\ndisplay_name: T\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
        )
        .unwrap();
        std::fs::write(
            org_root.join("people.yml"),
            "people:\n  - github_user_id: 42\n    github_username: alice\n",
        )
        .unwrap();
        std::fs::write(
            org_root.join("teams.yml"),
            "teams:\n  - name: default\n    parent_team: platform\n  - name: platform\n",
        )
        .unwrap();
        let parent = PaneTarget::Team {
            org: ORG.into(),
            team: "platform".into(),
        };
        write(home.path(), &principal, &parent, "roadmap", "x").unwrap();
        std::fs::create_dir_all(org_root.join("platform/config")).unwrap();
        std::fs::write(
            org_root.join("platform/config/sharing.yml"),
            "share: ['panes/roadmap.html']\n",
        )
        .unwrap();
        let ToolEnvelope::Ok(opened) = open(
            home.path(),
            &principal,
            &team_target(),
            "roadmap",
            Some((ORG, "default")),
            None,
        )
        .unwrap() else {
            panic!("an inherited pane opens by id")
        };
        assert_eq!(opened.panel_id, "pane:acme/api:platform/roadmap");
        assert!(opened.placed);
    }

    #[test]
    fn open_passes_intent_and_caps_serialized_utf8_bytes() {
        let (home, principal) = setup();
        let target = team_target();
        write(
            home.path(),
            &principal,
            &target,
            "inbox",
            "<title>Inbox</title>",
        )
        .unwrap();
        let intent = serde_json::json!({"conversation":"Mia", "messageId":7})
            .as_object()
            .unwrap()
            .clone();
        let ToolEnvelope::Ok(opened) = open(
            home.path(),
            &principal,
            &target,
            "inbox",
            Some((ORG, "default")),
            Some(intent.clone()),
        )
        .unwrap() else {
            panic!("open ok")
        };
        assert_eq!(opened.intent, Some(intent.clone()));
        assert_eq!(
            serde_json::to_value(opened).unwrap()["intent"],
            serde_json::json!(intent)
        );
        for size in [4096, 4097] {
            let intent = serde_json::json!({"x": "a".repeat(size - 8)})
                .as_object()
                .unwrap()
                .clone();
            let result = open(
                home.path(),
                &principal,
                &target,
                "inbox",
                Some((ORG, "default")),
                Some(intent),
            )
            .unwrap();
            assert_eq!(matches!(result, ToolEnvelope::Ok(_)), size == 4096);
        }
        let intent = serde_json::json!({"x": "📝".repeat(1024)})
            .as_object()
            .unwrap()
            .clone();
        assert!(matches!(
            open(
                home.path(),
                &principal,
                &target,
                "inbox",
                None,
                Some(intent)
            )
            .unwrap(),
            ToolEnvelope::Validation { .. }
        ));
    }

    #[test]
    fn bad_ids_unsynced_teams_and_missing_panes_are_validation() {
        let (home, principal) = setup();
        for bad in ["Board", "-x", "a/b", "", "x.html"] {
            assert!(matches!(
                write(home.path(), &principal, &team_target(), bad, "x").unwrap(),
                ToolEnvelope::Validation { .. }
            ));
        }
        let elsewhere = PaneTarget::Team {
            org: ORG.into(),
            team: "nope".into(),
        };
        assert!(matches!(
            write(home.path(), &principal, &elsewhere, "x", "x").unwrap(),
            ToolEnvelope::Validation { .. }
        ));
        assert!(matches!(
            open(
                home.path(),
                &principal,
                &team_target(),
                "absent",
                None,
                None
            )
            .unwrap(),
            ToolEnvelope::Validation { .. }
        ));
    }
}

#[cfg(test)]
mod caller_scope_tests;
