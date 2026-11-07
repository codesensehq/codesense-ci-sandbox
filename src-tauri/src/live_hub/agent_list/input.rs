// SPDX-License-Identifier: MIT
use super::{
    rows,
    tree::{TreeInput, TreeSession},
    workspace,
};
use crate::mcp::terminal_tabs::LiveTabs;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

pub(crate) struct Resolved {
    pub sessions: Vec<Value>,
    pub rows: Vec<super::AgentListRow>,
    pub home_team: String,
    pub team_order: Vec<String>,
    pub pane: Value,
}
fn story_of(ws: &Value, team: &str) -> Value {
    if ws["kind"] != "story"
        || ws["status"] != "ready"
        || !ws["storyStatus"].is_string()
        || !ws["storyId"].is_string()
        || !ws["storyFilePath"].is_string()
    {
        return Value::Null;
    }
    let mut story = json!({"storyId":ws["storyId"],"status":ws["storyStatus"],"storyFilePath":ws["storyFilePath"],"teamKey":team,"workspaceId":ws["id"]});
    if let Some(title) = ws.get("storyTitle") {
        story["title"] = title.clone();
    }
    story
}
fn reference(ws: &Value) -> Value {
    let nonempty = |key: &str| ws[key].as_str().filter(|s| !s.is_empty());
    let label = if ws["kind"] == "story" {
        nonempty("storyId").or_else(|| nonempty("storyTitle"))
    } else {
        nonempty("storyTitle").or_else(|| nonempty("contextWorktreeName"))
    };
    let fallback = if ws["kind"] == "generic" {
        ws["cwd"]
            .as_str()
            .unwrap_or_default()
            .rsplit('/')
            .find(|s| !s.is_empty())
            .or_else(|| nonempty("cwd"))
            .or_else(|| nonempty("id"))
            .unwrap_or_default()
            .to_string()
    } else {
        nonempty("id").unwrap_or_default().to_string()
    };
    let mut value = json!({"id":ws["id"],"label":label.unwrap_or(&fallback),"kind":if ws["kind"] == "story" {"story"} else {"folder"}});
    for key in [
        "storyStatus",
        "storyTitle",
        "sourceRepoSlug",
        "stageRole",
        "stageLabel",
    ] {
        if let Some(v) = ws.get(key) {
            value[key] = v.clone();
        }
    }
    if ws["location"]["kind"] == "machine" && ws["location"]["machineId"] != "local" {
        value["machineId"] = ws["location"]["machineId"].clone();
    }
    value
}
pub(super) fn cli_label(cli: &str) -> String {
    codesense_pty_core::agent_cli::known_label(cli)
        .map(str::to_string)
        .unwrap_or_else(|| {
            let mut chars = cli.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_else(|| "Agent".into())
        })
}
pub(crate) fn resolve(
    viewer: &Value,
    sessions: Vec<Value>,
    live: &LiveTabs,
    layouts: Value,
    identity: &Value,
    definitions: &BTreeMap<String, Value>,
    processes: &BTreeMap<String, String>,
    home_team: String,
    stories: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
    progress_note: &dyn Fn(&str) -> Option<String>,
) -> Resolved {
    let mut src = json!({"workspaces":viewer["workspaces"],"softClosedWorkspaces":viewer["softClosedWorkspaces"],"running":{},"minimized":{},"hidden":{},"layoutByKey":layouts,"byKey":{},"storyIndex":{}});
    let open_count = src["workspaces"]
        .as_object()
        .into_iter()
        .flat_map(|teams| teams.values())
        .filter_map(Value::as_array)
        .map(Vec::len)
        .sum::<usize>();
    let mut story_priorities = BTreeMap::new();
    for (index, (team, mut ws)) in workspace::entries(&src).into_iter().enumerate() {
        if let Some(entry) = ws["storyFilePath"]
            .as_str()
            .and_then(|path| stories.and_then(|s| s.entries.get(std::path::Path::new(path))))
        {
            if let Some(status) = crate::install::frontmatter_cache::CacheEntry::status(entry) {
                ws["storyStatus"] = status.into();
            }
            if let Some(id) = entry.story_id() {
                ws["storyId"] = id.into();
            }
        }
        if let Some(state) = viewer["agentStageStates"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|state| state["id"] == ws["storyStatus"])
        {
            ws["stageRole"] = state["role"].clone();
            ws["stageLabel"] = state["label"].clone();
        }
        let id = ws["id"].as_str().unwrap_or_default();
        src["byKey"][format!("{team}|{id}")] = ws.clone();
        let story = story_of(&ws, &team);
        if let Some(id) = story["storyId"].as_str() {
            let priority = (
                usize::from(index >= open_count),
                usize::from(team != home_team),
            );
            if story_priorities.get(id).is_none_or(|old| priority < *old) {
                story_priorities.insert(id.to_string(), priority);
                src["storyIndex"][id] = story.clone();
            }
        }
    }
    for tab in &live.tabs {
        let Some((team, workspace)) = tab.workspace_key.split_once('|') else {
            continue;
        };
        if !src["running"][team].is_object() {
            src["running"][team] = json!({});
        }
        if !src["running"][team][workspace].is_array() {
            src["running"][team][workspace] = json!([]);
        }
        src["running"][team][workspace].as_array_mut().unwrap().push(json!({"sessionId":tab.session,"name":tab.name,"origin":{"machineId":tab.machine_id},"exit":tab.exited}));
    }
    let mut detected = json!({});
    for (sid, cli) in processes {
        detected[sid] = json!({"cli":cli,"dot":null,"processOnly":true});
    }
    for (_, snapshots) in &live.activity {
        if let Ok(snapshots) = snapshots {
            for snapshot in snapshots.iter().filter(|s| s.observed && s.exit.is_none()) {
                detected[&snapshot.session] = json!({"cli":snapshot.provider.as_str(),"dot":null});
            }
        }
    }
    let mut sessions =
        super::roster::with_hand_started(&json!(sessions), &[src["running"].clone()], &detected)
            .as_array()
            .cloned()
            .unwrap_or_default();
    let mut input = TreeInput {
        team_key: home_team.clone(),
        tree_sessions: Vec::new(),
    };
    for row in &mut sessions {
        let handle = row["handle"].as_str().unwrap_or_default().to_string();
        let sid = row["sessionId"].as_str().unwrap_or_default();
        let tab = live.tabs.iter().find(|t| t.session == sid);
        let key = row["agentRevision"]
            .as_str()
            .and_then(|s| s.rsplit_once('@'))
            .map(|(key, _)| key);
        let def = key.and_then(|key| definitions.get(key));
        let cli = detected[sid]["cli"]
            .as_str()
            .or_else(|| def.and_then(|d| d["runtime"].as_str()));
        let renamed = identity
            .as_object()
            .into_iter()
            .flat_map(|o| o.values())
            .find_map(|team| team[sid]["name"].as_str());
        let cli_name = cli
            .filter(|_| {
                tab.is_some_and(|t| !t.name.is_empty())
                    || def.is_some()
                    || row["kind"] == "hand-started"
            })
            .map(cli_label);
        let progress = progress_note(sid);
        let detection = detected.get(sid).cloned();
        if let Some(detection) = detection {
            row["detected"] = detection;
        }
        row["names"] = json!({"renamed":renamed,"progress":progress,"live":tab.map(|t|&t.name),"saved":def.map(|d|&d["name"]),"cli":cli_name});
        let mut home = workspace::pane_workspace(row, &src);
        if let (Some(team), Some(id)) = (home["teamKey"].as_str(), home["workspace"]["id"].as_str())
        {
            if let Some(cached) = src["byKey"].get(format!("{team}|{id}")) {
                home["workspace"] = cached.clone();
            }
        }
        let has_home = row["workspace"].as_str().is_some_and(|s| !s.is_empty())
            || row["story"].as_str().is_some_and(|s| !s.is_empty());
        let story = if has_home {
            home["teamKey"]
                .as_str()
                .map(|team| story_of(&home["workspace"], team))
                .unwrap_or(Value::Null)
        } else if !handle.contains('/') && !handle.contains("::") {
            src["storyIndex"][handle.split('#').next().unwrap_or(&handle)].clone()
        } else {
            Value::Null
        };
        let team = has_home
            .then(|| home["teamKey"].as_str().map(str::to_string))
            .flatten();
        let managers: HashMap<_, _> = identity
            .as_object()
            .into_iter()
            .flatten()
            .map(|(team, choices)| {
                (
                    team.clone(),
                    choices[&handle]["manager"].as_str().map(str::to_string),
                )
            })
            .collect();
        input.tree_sessions.push(TreeSession {
            row: row.clone(),
            story,
            workspace: home["workspace"]
                .is_object()
                .then(|| reference(&home["workspace"]))
                .unwrap_or(Value::Null),
            team,
            tab_team: tab
                .and_then(|t| t.workspace_key.split_once('|'))
                .map(|(t, _)| t.into()),
            manager: identity[&home_team][&handle]["manager"]
                .as_str()
                .map(str::to_string),
            managers,
        });
    }
    let mut rows = rows::rows(&input, &viewer["resolvedWorkspaceOrderByTeam"]);
    rows.sort_by(|a, b| {
        super::team_sort_key(viewer, &a.team_key)
            .cmp(&super::team_sort_key(viewer, &b.team_key))
            .then_with(|| a.order.partial_cmp(&b.order).unwrap())
    });
    for (index, row) in rows.iter_mut().enumerate() {
        row.order = index as f64;
    }
    let mut team_order: Vec<_> = viewer["openedTeams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|team| *team != "_no_team:_none")
        .map(str::to_string)
        .chain(rows.iter().map(|row| row.team_key.clone()))
        .collect();
    team_order.sort_by_key(|team| super::team_sort_key(viewer, team));
    team_order.dedup();
    let mut pane = viewer["agentPane"].as_object().cloned().unwrap_or_default();
    pane.insert(
        "workspaceOrder".into(),
        viewer["resolvedWorkspaceOrderByTeam"]
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into(),
    );
    Resolved {
        pane: pane.into(),
        rows,
        sessions,
        home_team,
        team_order,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pane_order_comes_from_the_resolved_tray_order() {
        let viewer = json!({"agentPane":{"rails":{"o:a":[]}},
            "resolvedWorkspaceOrderByTeam":{"o:a":["second","first"],"o:b":["private"]}});
        let live = LiveTabs {
            tabs: vec![],
            offline: vec![],
            live: vec![],
            labels: Default::default(),
            activity: vec![],
        };
        let resolved = resolve(
            &viewer,
            vec![],
            &live,
            json!({}),
            &json!({}),
            &Default::default(),
            &Default::default(),
            "o:a".into(),
            None,
            &|_| None,
        );
        assert_eq!(
            resolved.pane["workspaceOrder"],
            viewer["resolvedWorkspaceOrderByTeam"]
        );
        assert_eq!(resolved.pane["rails"], viewer["agentPane"]["rails"]);
    }
}
