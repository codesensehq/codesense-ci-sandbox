// SPDX-License-Identifier: MIT
//! Personal workspace groups. All UI and MCP edits serialize through this boundary.
use super::state::McpViewerState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use tauri::{Emitter, Manager};

static EDIT: Mutex<()> = Mutex::new(());
static REVISION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceGroup {
    pub id: String,
    pub name: String,
    pub workspace_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GroupParams {
    #[serde(alias = "team_key")]
    pub team_key: Option<String>,
    #[serde(alias = "group_id")]
    pub group_id: Option<String>,
    pub name: Option<String>,
    #[serde(alias = "workspace_ids")]
    pub workspace_ids: Option<Vec<String>>,
    #[serde(alias = "story_ids")]
    pub story_ids: Option<Vec<String>>,
    #[serde(alias = "workspace_id")]
    pub workspace_id: Option<String>,
    #[serde(alias = "story_id")]
    pub story_id: Option<String>,
}

fn validation(message: impl Into<String>) -> Value {
    json!({"status":"validation","message":message.into()})
}
fn name_problem(groups: &[WorkspaceGroup], name: &str, skip: Option<&str>) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some("Type a name, or press Esc to cancel.".into());
    }
    if name.chars().count() > 80 {
        return Some("Keep the name under 80 characters.".into());
    }
    groups
        .iter()
        .find(|g| Some(g.id.as_str()) != skip && g.name.to_lowercase() == name.to_lowercase())
        .map(|g| format!("There's already a group called \"{}\".", g.name))
}

pub fn reduce(
    groups: &mut Vec<WorkspaceGroup>,
    edit: &Value,
    rows: &[Value],
) -> Result<Option<WorkspaceGroup>, String> {
    let action = edit["action"].as_str().unwrap_or("");
    let id = edit["groupId"].as_str().unwrap_or("");
    let at = groups.iter().position(|g| g.id == id);
    if matches!(
        action,
        "rename" | "set_members" | "add" | "remove" | "delete"
    ) && at.is_none()
    {
        return Err("Unknown group.".into());
    }
    let name = edit["name"].as_str().unwrap_or("").trim();
    if matches!(action, "create" | "rename") {
        if let Some(problem) = name_problem(
            groups,
            name,
            if action == "rename" { Some(id) } else { None },
        ) {
            return Err(problem);
        }
    }
    let mut members: Vec<String> =
        serde_json::from_value(edit.get("workspaceIds").cloned().unwrap_or(json!([])))
            .map_err(|_| "Unknown workspace.")?;
    if let Some(id) = edit["workspaceId"].as_str() {
        members.push(id.into());
    }
    let stories: Vec<String> = edit["storyIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .chain(edit["storyId"].as_str().map(str::to_string))
        .collect();
    for story in stories {
        let row = rows
            .iter()
            .find(|w| {
                w["storyId"].as_str() == Some(&story)
                    || w["id"].as_str() == Some(&format!("story:{story}"))
            })
            .ok_or("Unknown workspace.")?;
        members.push(row["id"].as_str().ok_or("Unknown workspace.")?.into());
    }
    if matches!(action, "create" | "set_members" | "add") {
        for member in &members {
            if at.is_some_and(|index| groups[index].workspace_ids.contains(member)) {
                continue;
            }
            let row = rows
                .iter()
                .find(|w| w["id"].as_str() == Some(member))
                .ok_or("Unknown workspace.")?;
            if row["kind"] == "root" {
                return Err("Repositories stay in their own group.".into());
            }
        }
        let mut seen = std::collections::HashSet::new();
        members.retain(|id| seen.insert(id.clone()));
    }
    if action == "add" {
        let mut current = groups[at.unwrap()].workspace_ids.clone();
        for member in members {
            if !current.contains(&member) {
                current.push(member);
            }
        }
        members = current;
    } else if action == "remove" {
        for member in &members {
            if !groups[at.unwrap()].workspace_ids.contains(member)
                && !rows.iter().any(|w| w["id"].as_str() == Some(member))
            {
                return Err("Unknown workspace.".into());
            }
        }
        members = groups[at.unwrap()]
            .workspace_ids
            .iter()
            .filter(|id| !members.contains(id))
            .cloned()
            .collect();
    }
    let originally_nonempty: Vec<String> = groups
        .iter()
        .filter(|g| !g.workspace_ids.is_empty())
        .map(|g| g.id.clone())
        .collect();
    let result = match action {
        "create" => {
            let group = WorkspaceGroup {
                id: uuid::Uuid::new_v4().to_string(),
                name: name.into(),
                workspace_ids: members.clone(),
            };
            groups.push(group.clone());
            Some(group)
        }
        "rename" => {
            let group = &mut groups[at.unwrap()];
            group.name = name.into();
            Some(group.clone())
        }
        "set_members" | "add" | "remove" => {
            let group = &mut groups[at.unwrap()];
            group.workspace_ids = members.clone();
            Some(group.clone())
        }
        "delete" => Some(groups.remove(at.unwrap())),
        "restore" => {
            let group: WorkspaceGroup =
                serde_json::from_value(edit["group"].clone()).map_err(|_| "Unknown group.")?;
            if groups.iter().any(|g| g.id == group.id) {
                return Err("Group already exists.".into());
            }
            if let Some(problem) = name_problem(groups, &group.name, None) {
                return Err(problem);
            }
            members = group.workspace_ids.clone();
            let index =
                (edit["index"].as_u64().unwrap_or(groups.len() as u64) as usize).min(groups.len());
            groups.insert(index, group.clone());
            Some(group)
        }
        _ => return Err("Unknown group action.".into()),
    };
    if matches!(
        action,
        "create" | "set_members" | "add" | "remove" | "restore"
    ) {
        let target = &result.as_ref().unwrap().id;
        if action != "remove" {
            for group in groups.iter_mut().filter(|g| &g.id != target) {
                group.workspace_ids.retain(|id| !members.contains(id));
            }
        }
        if action != "restore" && edit["pruneEmpty"] == true {
            groups.retain(|g| {
                !g.workspace_ids.is_empty()
                    || (!originally_nonempty.contains(&g.id) && &g.id != target)
            });
        }
    }
    if action == "restore" {
        if let Some(prune_id) = edit["pruneGroupId"].as_str() {
            groups.retain(|group| group.id != prune_id || !group.workspace_ids.is_empty());
        }
    }
    Ok(result)
}

/// A stale renderer save must not overwrite an edit made while it was busy.
/// Team rename/close still moves/removes both team-keyed slices together.
pub fn reconcile_snapshot(current: &Value, incoming: &mut Value) {
    if let Some(teams) = current["customGroupsByTeam"].as_object() {
        for (team, groups) in teams {
            let removed = incoming["openedTeams"]
                .as_array()
                .is_some_and(|teams| !teams.iter().any(|t| t.as_str() == Some(team)))
                && incoming["workspaces"].get(team).is_none();
            let version = current["workspaceGroupVersionsByTeam"][team]
                .as_u64()
                .unwrap_or(0);
            let next = incoming["workspaceGroupVersionsByTeam"][team]
                .as_u64()
                .unwrap_or(0);
            if !removed && next < version {
                if !incoming["customGroupsByTeam"].is_object() {
                    incoming["customGroupsByTeam"] = json!({});
                }
                if !incoming["workspaceGroupVersionsByTeam"].is_object() {
                    incoming["workspaceGroupVersionsByTeam"] = json!({});
                }
                incoming["customGroupsByTeam"][team] = groups.clone();
                incoming["workspaceGroupVersionsByTeam"][team] = json!(version);
            }
        }
    }
}

pub fn apply(
    app: &tauri::AppHandle,
    team: &str,
    edit: Value,
    expected: Option<&str>,
) -> Result<Value, String> {
    let _edit = EDIT.lock().map_err(|_| "Workspace groups unavailable")?;
    let profile = app.state::<crate::commands::local_profile::LocalProfileState>();
    let principal = crate::commands::local_profile::active(app)?.principal;
    profile.with_expected(expected, || {
        let mirror = app.state::<McpViewerState>();
        let viewer = mirror.get();
        if !viewer["openedTeams"]
            .as_array()
            .is_some_and(|teams| teams.iter().any(|t| t.as_str() == Some(team)))
            && viewer["workspaces"].get(team).is_none()
            && viewer["softClosedWorkspaces"].get(team).is_none()
        {
            return Ok(validation("Unknown team."));
        }
        let mut rows = viewer["workspaces"][team]
            .as_array()
            .cloned()
            .unwrap_or_default();
        rows.extend(
            viewer["softClosedWorkspaces"][team]
                .as_object()
                .into_iter()
                .flat_map(|m| m.values())
                .map(|v| v["workspace"].clone()),
        );
        let mut groups: Vec<WorkspaceGroup> =
            serde_json::from_value(viewer["customGroupsByTeam"][team].clone()).unwrap_or_default();
        if edit["action"] == "list" {
            return Ok(json!({"status":"ok","teamKey":team,"groups":groups}));
        }
        let group = match reduce(&mut groups, &edit, &rows) {
            Ok(group) => group,
            Err(message) => return Ok(validation(message)),
        };
        let previous = viewer["workspaceGroupVersionsByTeam"]
            .as_object()
            .into_iter()
            .flat_map(|v| v.values())
            .filter_map(Value::as_u64)
            .max()
            .unwrap_or(0);
        REVISION.fetch_max(previous, Ordering::SeqCst);
        let revision = REVISION.fetch_add(1, Ordering::SeqCst) + 1;
        let group_value = serde_json::to_value(&groups).map_err(|e| e.to_string())?;
        crate::commands::system::save_workspace_groups(
            app,
            principal.as_str(),
            team,
            group_value.clone(),
            revision,
        )?;
        mirror.replace_workspace_groups(team, group_value, revision);
        let result =
            json!({"status":"ok","teamKey":team,"groups":groups,"group":group,"revision":revision});
        app.emit("mcp:workspace-groups-changed", &result)
            .map_err(|e| e.to_string())?;
        Ok(result)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn group() -> WorkspaceGroup {
        WorkspaceGroup {
            id: "g".into(),
            name: "Checks".into(),
            workspace_ids: vec!["a".into()],
        }
    }
    #[test]
    fn concurrent_mcp_add_and_ui_move_keep_both_members() {
        let rows = vec![
            json!({"id":"a","kind":"story"}),
            json!({"id":"b","kind":"story"}),
            json!({"id":"c","kind":"generic"}),
        ];
        let ui_move = json!({"action":"add","groupId":"g","workspaceId":"c","pruneEmpty":true});
        let mcp_add = json!({"action":"add","groupId":"g","workspaceId":"b"});
        for edits in [[&mcp_add, &ui_move], [&ui_move, &mcp_add]] {
            let mut groups = vec![group()];
            for edit in edits {
                reduce(&mut groups, edit, &rows).unwrap();
            }
            let mut members = groups[0].workspace_ids.clone();
            members.sort();
            assert_eq!(members, vec!["a", "b", "c"]);
        }
    }
    #[test]
    fn editing_a_group_keeps_closed_members_and_can_remove_them() {
        let mut groups = vec![group()];
        let rows = vec![
            json!({"id":"b","kind":"story"}),
            json!({"id":"c","kind":"generic"}),
        ];
        reduce(
            &mut groups,
            &json!({"action":"add","groupId":"g","workspaceId":"b"}),
            &rows,
        )
        .unwrap();
        reduce(
            &mut groups,
            &json!({"action":"set_members","groupId":"g","workspaceIds":["a","b","c"]}),
            &rows,
        )
        .unwrap();
        reduce(
            &mut groups,
            &json!({"action":"remove","groupId":"g","workspaceId":"b"}),
            &rows,
        )
        .unwrap();
        assert_eq!(groups[0].workspace_ids, vec!["a", "c"]);
        reduce(
            &mut groups,
            &json!({"action":"remove","groupId":"g","workspaceId":"a"}),
            &rows,
        )
        .unwrap();
        assert_eq!(groups[0].workspace_ids, vec!["c"]);
    }
    #[test]
    fn undo_move_preserves_a_preexisting_empty_destination() {
        let source = group();
        let reserved = WorkspaceGroup {
            id: "reserved".into(),
            name: "Reserved".into(),
            workspace_ids: vec![],
        };
        let mut groups = vec![source.clone(), reserved.clone()];
        let rows = vec![json!({"id":"a","kind":"story"})];
        reduce(
            &mut groups,
            &json!({"action":"add","groupId":"reserved","workspaceId":"a","pruneEmpty":true}),
            &rows,
        )
        .unwrap();
        assert_eq!(groups.len(), 1);
        reduce(
            &mut groups,
            &json!({"action":"restore","group":source,"index":0}),
            &rows,
        )
        .unwrap();
        assert_eq!(groups[0].id, "g");
        assert_eq!(groups[0].workspace_ids, vec!["a"]);
        assert_eq!(groups[1], reserved);
    }
    #[test]
    fn new_group_prunes_the_emptied_source_and_undo_restores_its_position() {
        let source = group();
        let mut groups = vec![
            source.clone(),
            WorkspaceGroup {
                id: "empty".into(),
                name: "Reserved".into(),
                workspace_ids: vec![],
            },
        ];
        let rows = vec![json!({"id":"a","kind":"story"})];
        let new = reduce(
            &mut groups,
            &json!({"action":"create","name":"New","workspaceIds":["a"],"pruneEmpty":true}),
            &rows,
        )
        .unwrap()
        .unwrap();
        assert!(!groups.iter().any(|g| g.id == source.id));
        reduce(
            &mut groups,
            &json!({"action":"restore","group":source,"index":0,"pruneGroupId":new.id}),
            &rows,
        )
        .unwrap();
        assert_eq!(groups[0].id, "g");
        assert_eq!(groups[0].workspace_ids, vec!["a"]);
        assert_eq!(groups[1].id, "empty");
        assert!(!groups.iter().any(|g| g.id == new.id));
    }
}
