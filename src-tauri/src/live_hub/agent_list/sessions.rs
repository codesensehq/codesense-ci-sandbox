// SPDX-License-Identifier: MIT
use super::AgentListRow;
use crate::live_hub::{inbox::INBOX_FIELDS, projection::FieldProviders};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn identity(row: &Value) -> &str {
    row["sessionId"]
        .as_str()
        .unwrap_or_else(|| row["handle"].as_str().unwrap_or_default())
}
fn utility(row: &Value) -> bool {
    matches!(
        row["kind"].as_str(),
        Some("live-pty" | "root" | "notes" | "#")
    )
}

/// Join one process owner by its bound session id. Handles remain display/address metadata.
pub(crate) fn project(
    roster: &[Value],
    runtime: &[Value],
    references: &mut [AgentListRow],
    providers: &FieldProviders,
) -> Vec<(Option<String>, Value)> {
    let mut owners = BTreeMap::<String, Value>::new();
    for row in roster {
        let id = identity(row).to_string();
        if owners
            .get(&id)
            .is_none_or(|old| utility(old) && !utility(row))
        {
            owners.insert(id, row.clone());
        }
    }
    for row in runtime {
        let id = identity(row).to_string();
        owners.entry(id).or_insert_with(|| row.clone());
    }
    owners
        .into_iter()
        .map(|(id, mut metadata)| {
            let live = runtime.iter().find(|row| identity(row) == id).or_else(|| {
                runtime
                    .iter()
                    .find(|row| row["handle"] == metadata["handle"])
            });
            let mut source = live.and_then(Value::as_object).cloned().unwrap_or_default();
            let display = source.get("displayName").cloned();
            source.extend(metadata.as_object().unwrap().clone());
            if let Some(display) = display {
                source.insert("displayName".into(), display);
            }
            source.insert("id".into(), id.clone().into());
            if !source.contains_key("state") {
                source.insert(
                    "state".into(),
                    if metadata["alive"] == false {
                        "exited"
                    } else {
                        "unknown"
                    }
                    .into(),
                );
            }
            for key in INBOX_FIELDS {
                source.remove(*key);
            }
            let reference = references.iter_mut().find(|row| row.session_ref == id);
            if let Some(reference) = reference {
                if let (Some(org), Some(team)) = (
                    source.get("team").and_then(|v| v["org"].as_str()),
                    source.get("team").and_then(|v| v["team"].as_str()),
                ) {
                    reference.inbox_ref = format!("counts:team:{org}/{team}");
                }
            }
            let scope = source
                .get("team")
                .filter(|v| v.is_object())
                .and_then(|team| {
                    Some(format!(
                        "team:{}/{}",
                        team["org"].as_str()?,
                        team["team"].as_str()?
                    ))
                })
                .or_else(|| {
                    references
                        .iter()
                        .find(|row| row.session_ref == id)
                        .map(|row| format!("team:{}", row.team_key.replacen(':', "/", 1)))
                });
            if source.get("team").is_none_or(Value::is_null) {
                if let Some(team) = scope
                    .as_deref()
                    .and_then(|scope| scope.strip_prefix("team:"))
                    .and_then(|team| team.rsplit_once('/'))
                {
                    source.insert("team".into(), json!({"org":team.0,"team":team.1}));
                }
            }
            let mut projected = providers.project(&source, source.clone());
            metadata["effectiveStatus"] = projected["effectiveStatus"].clone();
            if projected["model"].is_null() {
                if let Some(cli) = metadata["detected"]["cli"].as_str() {
                    projected["model"] = super::input::cli_label(cli).into();
                }
            }
            projected["rowNames"] = super::names::row_names(&metadata);
            projected.as_object_mut().unwrap().remove("_statusSignals");
            projected.as_object_mut().unwrap().remove("_managedOutcome");
            projected.as_object_mut().unwrap().remove("_namesAnAgent");
            projected
                .as_object_mut()
                .unwrap()
                .remove("_pendingManagedAsk");
            (scope, projected)
        })
        .collect()
}
