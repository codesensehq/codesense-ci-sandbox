// SPDX-License-Identifier: MIT
//! The shared MCP tool result envelope and the ambient-default / any-team-override context
//! convention every team-scoped tool uses: no `org`/`team` args target the active team, both args
//! target that member team without switching to it.

use serde::Serialize;
use tauri::AppHandle;

use crate::commands::route_to_context::{
    derive_active_context, derive_context_for, ActiveContext, ContextResolution,
};

/// The one tagged result union MCP tools return, so an agent can branch on `status` instead of
/// parsing a stringified error. Adjacently tagged (`{ "status", "data" }`) because many `ok`
/// payloads are lists, which serde cannot merge an internal tag into.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", content = "data", rename_all = "camelCase")]
pub(crate) enum ToolEnvelope<T> {
    /// The tool's payload (a list, a record, a report — whatever the tool returns).
    Ok(T),
    /// The targeted team is a member team but its content dir is absent locally (`NotSynced`).
    NotSyncedLocally,
    /// The targeted `(org, team)` is not confirmable from the user's locally-synced memberships
    /// (`Unauthorized`).
    UnauthorizedTeam,
    /// The tool's inputs were malformed (bad arg shape, unknown id, an invalid transform).
    Validation { message: String },
    /// A write was made against a stale revision and changed nothing. `current_revision` is the
    /// revision to re-read and retry from.
    Conflict {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        current_revision: Option<u64>,
    },
}

/// The resolved target of a tool's optional `org`/`team` args. `Resolved` boxes the (large)
/// [`ActiveContext`] so the enum stays small.
pub(crate) enum ContextTarget {
    Resolved(Box<ActiveContext>),
    NotSyncedLocally,
    UnauthorizedTeam,
    Validation(String),
}

/// The shape of a tool's `(org, team)` args, classified before any async resolution — pure so the
/// three branches are unit-testable without an `AppHandle`.
enum ArgShape {
    /// Neither arg → the running instance's active team.
    Active,
    /// Both args → resolve that specific member team (active or not).
    Target(String, String),
    /// Exactly one arg, or a blank arg → a validation error.
    Invalid(String),
}

/// The ambient-default / any-team-override convention, classified. No arg → active;
/// both → target; one-of / blank → invalid. The one rule every team-scoped MCP tool shares.
fn classify_args(org: Option<String>, team: Option<String>) -> ArgShape {
    match (org, team) {
        (None, None) => ArgShape::Active,
        (Some(org), Some(team)) => {
            if org.trim().is_empty() || team.trim().is_empty() {
                ArgShape::Invalid("org and team must be non-empty".to_string())
            } else {
                ArgShape::Target(org, team)
            }
        }
        _ => ArgShape::Invalid(
            "provide BOTH org and team to target a specific team, or NEITHER to use the active team"
                .to_string(),
        ),
    }
}

/// Map the [`ContextResolution`] onto the tool's [`ContextTarget`] — pure.
fn map_resolution(res: ContextResolution) -> ContextTarget {
    match res {
        ContextResolution::Ok(ctx) => ContextTarget::Resolved(Box::new(ctx)),
        ContextResolution::Unauthorized => ContextTarget::UnauthorizedTeam,
        ContextResolution::NotSynced => ContextTarget::NotSyncedLocally,
    }
}

/// Resolve a tool's `(org, team)` args to a concrete team context WITHOUT switching
/// the app's active team. No arg → [`derive_active_context`]; both args →
/// [`derive_context_for`] (the any-team resolver, bounded to the user's locally-synced
/// memberships). Returns the shared [`ContextTarget`] so every caller maps outcomes onto the ONE
/// envelope identically. `Err` is reserved for a hard precondition failure (no signed-in user, home
/// dir unavailable) that surfaces as an MCP error, never a typed envelope arm.
pub(crate) async fn resolve_context(
    app: &AppHandle,
    org: Option<String>,
    team: Option<String>,
) -> Result<ContextTarget, String> {
    match classify_args(org, team) {
        ArgShape::Active => Ok(ContextTarget::Resolved(Box::new(
            derive_active_context(app).await?,
        ))),
        ArgShape::Target(org, team) => {
            Ok(map_resolution(derive_context_for(app, &org, &team).await?))
        }
        ArgShape::Invalid(message) => Ok(ContextTarget::Validation(message)),
    }
}

/// Split a resolved context out of a [`ContextTarget`], or fall through to the matching envelope arm
/// for a caller to return directly. Generic over the tool's `ok` payload `T` so the short-circuit
/// arms produce the right `ToolEnvelope<T>` without the tool naming them.
pub(crate) fn resolved_or_envelope<T>(
    target: ContextTarget,
) -> Result<ActiveContext, ToolEnvelope<T>> {
    match target {
        ContextTarget::Resolved(ctx) => Ok(*ctx),
        ContextTarget::NotSyncedLocally => Err(ToolEnvelope::NotSyncedLocally),
        ContextTarget::UnauthorizedTeam => Err(ToolEnvelope::UnauthorizedTeam),
        ContextTarget::Validation(message) => Err(ToolEnvelope::Validation { message }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every tag serializes to its own distinct, adjacently-tagged shape, never a stringified error.
    #[test]
    fn envelope_ok_carries_a_list_under_data() {
        let env = ToolEnvelope::Ok(vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            serde_json::to_value(&env).unwrap(),
            serde_json::json!({ "status": "ok", "data": ["a", "b"] })
        );
    }

    #[test]
    fn envelope_context_arms_are_bare_tags() {
        let not_synced: ToolEnvelope<()> = ToolEnvelope::NotSyncedLocally;
        let unauthorized: ToolEnvelope<()> = ToolEnvelope::UnauthorizedTeam;
        assert_eq!(
            serde_json::to_value(&not_synced).unwrap(),
            serde_json::json!({ "status": "notSyncedLocally" })
        );
        assert_eq!(
            serde_json::to_value(&unauthorized).unwrap(),
            serde_json::json!({ "status": "unauthorizedTeam" })
        );
    }

    #[test]
    fn envelope_validation_conflict_and_unauthorized_shapes() {
        let validation: ToolEnvelope<()> = ToolEnvelope::Validation {
            message: "bad input".into(),
        };
        let conflict: ToolEnvelope<()> = ToolEnvelope::Conflict {
            message: "stale".into(),
            current_revision: None,
        };
        let revisioned: ToolEnvelope<()> = ToolEnvelope::Conflict {
            message: "stale".into(),
            current_revision: Some(4),
        };
        assert_eq!(
            serde_json::to_value(&validation).unwrap(),
            serde_json::json!({ "status": "validation", "data": { "message": "bad input" } })
        );
        assert_eq!(
            serde_json::to_value(&conflict).unwrap(),
            serde_json::json!({ "status": "conflict", "data": { "message": "stale" } })
        );
        assert_eq!(
            serde_json::to_value(&revisioned).unwrap(),
            serde_json::json!({
                "status": "conflict",
                "data": { "message": "stale", "current_revision": 4 }
            })
        );
    }

    // The ambient-default / any-team-override rule, classified.
    #[test]
    fn classify_args_no_args_is_active() {
        assert!(matches!(classify_args(None, None), ArgShape::Active));
    }

    #[test]
    fn classify_args_both_args_is_target() {
        match classify_args(Some("acme/ctx".into()), Some("default".into())) {
            ArgShape::Target(org, team) => {
                assert_eq!(org, "acme/ctx");
                assert_eq!(team, "default");
            }
            _ => panic!("both args should classify as Target"),
        }
    }

    #[test]
    fn classify_args_partial_or_blank_is_invalid() {
        assert!(matches!(
            classify_args(Some("acme/ctx".into()), None),
            ArgShape::Invalid(_)
        ));
        assert!(matches!(
            classify_args(None, Some("default".into())),
            ArgShape::Invalid(_)
        ));
        assert!(matches!(
            classify_args(Some("  ".into()), Some("default".into())),
            ArgShape::Invalid(_)
        ));
    }

    // A context resolution maps onto the right envelope arm.
    #[test]
    fn map_resolution_covers_every_arm() {
        assert!(matches!(
            map_resolution(ContextResolution::Unauthorized),
            ContextTarget::UnauthorizedTeam
        ));
        assert!(matches!(
            map_resolution(ContextResolution::NotSynced),
            ContextTarget::NotSyncedLocally
        ));
        let ctx = ActiveContext {
            codesense_home: std::path::PathBuf::from("/home/.codesense"),
            user_id: 42,
            org_slug: "acme/ctx".into(),
            team_name: Some("default".into()),
        };
        assert!(matches!(
            map_resolution(ContextResolution::Ok(ctx)),
            ContextTarget::Resolved(_)
        ));
    }

    #[test]
    fn resolved_or_envelope_splits_resolved_from_short_circuits() {
        let ctx = ActiveContext {
            codesense_home: std::path::PathBuf::from("/home/.codesense"),
            user_id: 42,
            org_slug: "acme/ctx".into(),
            team_name: Some("default".into()),
        };
        let ok: Result<ActiveContext, ToolEnvelope<()>> =
            resolved_or_envelope(ContextTarget::Resolved(Box::new(ctx)));
        assert!(ok.is_ok());
        let short: Result<ActiveContext, ToolEnvelope<()>> =
            resolved_or_envelope(ContextTarget::UnauthorizedTeam);
        assert!(matches!(short, Err(ToolEnvelope::UnauthorizedTeam)));
    }
}
