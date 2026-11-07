// SPDX-License-Identifier: MIT
use serde_json::{json, Value};

/// Validate the whole request before a viewer field or private baseline can change.
pub(crate) fn validate_patch(
    patch: &Value,
    current: &Value,
    teams: &[String],
    has_profile: bool,
    mut resolve_worktree: impl FnMut(&str, &str) -> Result<String, String>,
) -> Result<Value, String> {
    let mut patch = patch.clone();
    let Some(lenses) = patch.get("navLens") else {
        return Ok(patch);
    };
    let lenses = lenses.as_object().ok_or("navLens must be an object")?;
    let mut cleaned = serde_json::Map::new();
    let mut reveal = patch
        .get("navCollapsed")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (team, lens) in lenses {
        if !teams.contains(team) {
            return Err(format!("No team {team}. Teams: {}.", teams.join(", ")));
        }
        let lens = lens
            .as_object()
            .ok_or("Each navLens entry must be an object")?;
        let mut entry = serde_json::Map::new();
        if let Some(source) = lens.get("source") {
            let source = source.as_str().ok_or("source must be a string")?;
            let source = match source.to_lowercase().as_str() {
                "clone" => "clone".to_string(),
                "private" if has_profile => "private".to_string(),
                "private" => {
                    return Err(
                        "You have no private space: you're not signed in to a profile.".into(),
                    )
                }
                _ => resolve_worktree(team, source)?,
            };
            entry.insert("source".into(), json!(source));
            reveal.insert(team.clone(), json!(false));
        }
        let worktree = entry
            .get("source")
            .and_then(Value::as_str)
            .is_some_and(|s| s != "clone" && s != "private");
        let view = lens.get("view").and_then(Value::as_str);
        if lens.contains_key("view") && !matches!(view, Some("files" | "changes" | "history")) {
            return Err("view must be files, changes or history".into());
        }
        entry.insert(
            "view".into(),
            json!(view
                .or_else(|| worktree.then_some("changes"))
                .or_else(|| current["navLens"][team]["view"].as_str())
                .unwrap_or("files")),
        );
        cleaned.insert(team.clone(), Value::Object(entry));
    }
    patch["navLens"] = Value::Object(cleaned);
    patch["navCollapsed"] = Value::Object(reveal);
    Ok(patch)
}

/// Resolve registered worktrees through the same validation owner as clone and Private.
pub(crate) fn validate_patch_at(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    patch: &Value,
    current: &Value,
    teams: &[String],
    has_profile: bool,
) -> Result<(Value, Value), String> {
    let mut descriptions = std::collections::HashMap::new();
    let validated = validate_patch(patch, current, teams, has_profile, |key, source| {
        let (org, team) = key.split_once(':').ok_or("teamKey must be org:team")?;
        let row = crate::install::worktree_index::resolve_context_record(
            home,
            principal,
            org,
            Some(team),
            source,
        )?;
        descriptions.insert(
            row.id.clone(),
            json!({"id": row.id, "name": row.name, "branch": row.branch}),
        );
        Ok(row.id)
    })?;
    let mut described = validated["navLens"].clone();
    if let Some(lenses) = described.as_object_mut() {
        for lens in lenses.values_mut() {
            if let Some(description) = lens["source"].as_str().and_then(|id| descriptions.get(id)) {
                lens["source"] = description.clone();
            }
        }
    }
    Ok((validated, described))
}

pub(crate) fn external_sources(viewer: &mut Value) {
    if let Some(lenses) = viewer.get_mut("navLens").and_then(Value::as_object_mut) {
        for lens in lenses.values_mut() {
            if let Some(source) = lens.get("source").filter(|s| s.is_object()).cloned() {
                lens["source"] = source
                    .get("id")
                    .or_else(|| source.get("kind"))
                    .cloned()
                    .unwrap_or(json!("clone"));
            } else if lens.get("source").is_none() {
                lens["source"] = json!("clone");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn set_viewer_state_rejects_bad_team_or_source_before_applying_anything() {
        let teams = vec!["acme/repo:dev".into()];
        for bad in [
            json!({"navLens":{"acme/repo:other":{"source":"private"}},"activeTab":{"x":"y"}}),
            json!({"navLens":{"acme/repo:dev":{"source":"missing"}}}),
        ] {
            assert!(validate_patch(&bad, &json!({}), &teams, true, |_, _| Err(
                "unknown source".into()
            ))
            .is_err());
        }
        assert!(validate_patch(
            &json!({"navLens":{"acme/repo:dev":{"source":"private"}}}),
            &json!({}),
            &teams,
            false,
            |_, _| Err("unknown source".into()),
        )
        .is_err());
    }
    #[test]
    fn reserved_sources_win_over_worktree_names_and_keep_the_current_view() {
        let key = "acme/repo:dev";
        for source in ["clone", "private"] {
            let patch = validate_patch(
                &json!({"navLens": {key: {"source": source}}}),
                &json!({"navLens": {key: {"view": "history"}}}),
                &[key.into()],
                true,
                |_, _| panic!("reserved word resolved as a worktree"),
            )
            .unwrap();
            assert_eq!(
                patch["navLens"][key],
                json!({"source": source, "view": "history"})
            );
        }
    }

    #[test]
    fn set_viewer_state_only_passes_source_and_view_and_reveals_nav() {
        let patch = json!({"navLens":{"acme/repo:dev":{"source":"private","lens":"done"}}});
        let current = json!({"navLens":{"acme/repo:dev":{"view":"history"}}});
        let result = validate_patch(&patch, &current, &["acme/repo:dev".into()], true, |_, _| {
            Err("unknown source".into())
        })
        .unwrap();
        assert_eq!(
            result["navLens"]["acme/repo:dev"],
            json!({"source":"private","view":"history"})
        );
        assert_eq!(result["navCollapsed"]["acme/repo:dev"], false);
    }
}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct SetPrivateItemsUseParams {
    pub org: String,
    pub team: String,
    pub r#use: bool,
}
