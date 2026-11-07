// SPDX-License-Identifier: MIT
use serde_json::{json, Value};

pub(super) fn entries(src: &Value) -> Vec<(String, Value)> {
    let mut entries = Vec::new();
    if let Some(teams) = src["workspaces"].as_object() {
        for (team, rows) in teams {
            if let Some(rows) = rows.as_array() {
                entries.extend(rows.iter().map(|ws| (team.clone(), ws.clone())));
            }
        }
    }
    if let Some(teams) = src["softClosedWorkspaces"].as_object() {
        for (team, rows) in teams {
            if let Some(rows) = rows.as_object() {
                entries.extend(
                    rows.values()
                        .map(|sc| (team.clone(), sc["workspace"].clone())),
                );
            }
        }
    }
    entries
}
fn location(sid: &str, src: &Value) -> Option<Value> {
    for kind in ["running", "minimized", "hidden"] {
        for (team, workspaces) in src[kind].as_object().into_iter().flatten() {
            for (id, tabs) in workspaces.as_object().into_iter().flatten() {
                if let Some(tab) = tabs
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|tab| tab["sessionId"] == sid)
                {
                    return Some(json!({"teamKey":team,"workspaceId":id,"tab":tab}));
                }
            }
        }
    }
    None
}
fn remote(machine: &Value) -> bool {
    machine
        .as_str()
        .is_some_and(|s| !s.is_empty() && s != "local")
}
fn normalize(dir: &str) -> &str {
    let s = dir.trim_end_matches('/');
    if s.is_empty() {
        "/"
    } else {
        s
    }
}
fn story_matches(ws: &Value, story: &str) -> bool {
    ws["storyId"] == story || ws["storyFilePath"] == story
}

pub(super) fn pane_workspace(row: &Value, src: &Value) -> Value {
    let loc = row["sessionId"]
        .as_str()
        .and_then(|sid| location(sid, src))
        .unwrap_or(Value::Null);
    let machine = row
        .get("machineId")
        .unwrap_or(&loc["tab"]["origin"]["machineId"]);
    let entries = entries(src);
    let want_dir = row["workspace"].as_str().filter(|s| !s.is_empty());
    let want_story = row["story"].as_str().filter(|s| !s.is_empty());
    let launch_team = row["launchTeamKey"].as_str().filter(|s| !s.is_empty());
    let preferred = launch_team.or_else(|| loc["teamKey"].as_str());
    let mut matches: Vec<_> = entries
        .iter()
        .filter(|(team, ws)| {
            if launch_team.is_some_and(|t| t != team) {
                return false;
            }
            let ws_remote = ws["location"]["kind"] == "machine";
            if remote(machine) {
                if !ws_remote || ws["location"]["machineId"] != *machine {
                    return false;
                }
            } else if ws_remote {
                return false;
            }
            let dir = if ws_remote {
                &ws["location"]["cwd"]
            } else {
                &ws["cwd"]
            };
            if let Some(want) = want_dir {
                dir.as_str()
                    .is_some_and(|s| !s.is_empty() && normalize(s) == normalize(want))
            } else {
                want_story.is_some_and(|s| story_matches(ws, s))
            }
        })
        .collect();
    let by_story: Vec<_> = matches
        .iter()
        .copied()
        .filter(|(_, ws)| want_story.is_some_and(|s| story_matches(ws, s)))
        .collect();
    if !by_story.is_empty() {
        matches = by_story;
    }
    if let Some((team, ws)) = matches
        .iter()
        .find(|(team, _)| Some(team.as_str()) == preferred)
        .copied()
        .or_else(|| matches.first().copied())
    {
        return json!({"teamKey":team,"workspace":ws});
    }
    if want_dir.is_some() || want_story.is_some() {
        return Value::Null;
    }
    if let (Some(team), Some(id)) = (loc["teamKey"].as_str(), loc["workspaceId"].as_str()) {
        let key = format!("{team}|{id}");
        if src["byKey"][&key].is_object() {
            return json!({"teamKey":team,"workspace":src["byKey"][&key]});
        }
        if !remote(&loc["tab"]["origin"]["machineId"]) {
            if let Some(dir) = src["layoutByKey"][&key]["descriptor"]["worktreePath"]
                .as_str()
                .filter(|s| !s.is_empty())
            {
                if let Some((_, ws)) = entries.iter().find(|(t, ws)| {
                    t == team
                        && ws["location"]["kind"] != "machine"
                        && ws["cwd"]
                            .as_str()
                            .is_some_and(|s| !s.is_empty() && normalize(s) == normalize(dir))
                }) {
                    return json!({"teamKey":team,"workspace":ws});
                }
            }
        }
    }
    let handle = row["handle"].as_str().unwrap_or_default();
    if !handle.contains('/') && !handle.contains("::") {
        let stem = handle.split('#').next().unwrap_or(handle);
        let story = &src["storyIndex"][stem];
        if let (Some(team), Some(id)) = (story["teamKey"].as_str(), story["workspaceId"].as_str()) {
            let ws = &src["byKey"][format!("{team}|{id}")];
            if ws.is_object() {
                return json!({"teamKey":team,"workspace":ws});
            }
        }
    }
    Value::Null
}
