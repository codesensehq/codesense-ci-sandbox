// SPDX-License-Identifier: MIT
use super::tree::{build_tree, other_groups, TreeInput};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// References into the session and inbox owners; never copies their presentation fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentListRow {
    pub id: String,
    pub session_ref: String,
    pub inbox_ref: String,
    pub team_key: String,
    pub placement_team_ref: Option<String>,
    pub parent_id: Option<String>,
    pub parent_source: Option<String>,
    pub order: f64,
    pub workspace_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub workspace: Value,
    pub story_ref: Option<String>,
    pub workspace_rank: usize,
    pub crew_parent_id: Option<String>,
    pub crew_order: Option<usize>,
    pub crew_lead: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_group: Option<u8>,
}

pub(super) fn rows(input: &TreeInput, workspace_orders: &Value) -> Vec<AgentListRow> {
    let mut teams = vec![(input.team_key.clone(), build_tree(input))];
    teams.extend(other_groups(input).into_iter().map(|g| {
        (
            g["teamKey"].as_str().unwrap().to_string(),
            g["nodes"].as_array().unwrap().clone(),
        )
    }));
    let mut out = Vec::new();
    for (team, nodes) in teams {
        let order = workspace_orders[&team]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let groups = super::tree::by_workspace(&nodes, order);
        fn walk(
            team: &str,
            nodes: &[Value],
            parent: Option<&str>,
            groups: &[Value],
            out: &mut Vec<AgentListRow>,
        ) {
            for node in nodes {
                let handle = node["row"]["handle"].as_str().unwrap_or_default();
                let id = serde_json::to_string(&(team, handle)).unwrap();
                let workspace = &node["workspace"];
                out.push(AgentListRow {
                    status_group: None,
                    id: id.clone(),
                    session_ref: node["row"]["sessionId"].as_str().unwrap_or(handle).into(),
                    inbox_ref: format!("counts:team:{}", team.replacen(':', "/", 1)),
                    team_key: team.into(),
                    placement_team_ref: None,
                    parent_id: parent.map(str::to_string),
                    parent_source: node["parentSource"].as_str().map(str::to_string),
                    order: out.len() as f64,
                    workspace: workspace.clone(),
                    workspace_ref: workspace["id"].as_str().map(str::to_string),
                    story_ref: node["story"]["workspaceId"].as_str().map(str::to_string),
                    crew_parent_id: None,
                    crew_order: None,
                    crew_lead: false,
                    workspace_rank: groups
                        .iter()
                        .position(|g| {
                            g["workspaceId"].as_str().unwrap_or_default()
                                == workspace["id"].as_str().unwrap_or_default()
                        })
                        .unwrap_or_default(),
                });
                walk(
                    team,
                    node["children"].as_array().unwrap(),
                    Some(&id),
                    groups,
                    out,
                );
            }
        }
        walk(&team, &nodes, None, &groups, &mut out);
    }
    for row in &mut out {
        row.placement_team_ref = input
            .tree_sessions
            .iter()
            .find(|session| {
                session.row["sessionId"]
                    .as_str()
                    .unwrap_or_else(|| session.row["handle"].as_str().unwrap_or_default())
                    == row.session_ref
            })
            .and_then(|session| super::tree::TreeSession::team(session).map(str::to_string));
    }
    let memberships: std::collections::BTreeSet<_> = out
        .iter()
        .filter_map(|row| {
            row.workspace_ref
                .as_ref()
                .map(|ws| (row.team_key.clone(), ws.clone()))
        })
        .collect();
    for (team, workspace) in memberships {
        let handles: std::collections::HashSet<_> = out
            .iter()
            .filter(|row| row.team_key == team && row.workspace_ref.as_deref() == Some(&workspace))
            .filter_map(|row| {
                input
                    .tree_sessions
                    .iter()
                    .find(|session| {
                        session.row["sessionId"]
                            .as_str()
                            .unwrap_or_else(|| session.row["handle"].as_str().unwrap_or_default())
                            == row.session_ref
                    })
                    .and_then(|session| session.row["handle"].as_str())
            })
            .collect();
        let sessions = input
            .tree_sessions
            .iter()
            .filter(|row| {
                handles.contains(row.row["handle"].as_str().unwrap_or_default())
                    && !super::tree::plain_shell(&row.row)
            })
            .cloned()
            .map(|mut row| {
                row.manager = None;
                row.managers.clear();
                row
            })
            .collect();
        let tree = build_tree(&TreeInput {
            team_key: team.clone(),
            tree_sessions: sessions,
        });
        let lead = tree
            .iter()
            .rev()
            .max_by_key(|node| super::tree::flatten(&[(*node).clone()]).len())
            .map(|node| node["row"]["handle"].as_str().unwrap().to_string());
        fn assign(
            nodes: &[Value],
            team: &str,
            parent: Option<&str>,
            lead: Option<&str>,
            index: &mut usize,
            out: &mut [AgentListRow],
        ) {
            for node in nodes {
                let handle = node["row"]["handle"].as_str().unwrap();
                if let Some(row) = out.iter_mut().find(|row| {
                    row.team_key == team
                        && row.id == serde_json::to_string(&(team, handle)).unwrap()
                }) {
                    row.crew_parent_id =
                        parent.map(|handle| serde_json::to_string(&(team, handle)).unwrap());
                    row.crew_order = Some(*index);
                    row.crew_lead = lead == Some(handle);
                }
                *index += 1;
                assign(
                    node["children"].as_array().unwrap(),
                    team,
                    Some(handle),
                    lead,
                    index,
                    out,
                );
            }
        }
        assign(&tree, &team, None, lead.as_deref(), &mut 0, &mut out);
    }
    out
}
