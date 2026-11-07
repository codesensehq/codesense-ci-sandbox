// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TreeSession {
    pub row: Value,
    #[serde(default)]
    pub story: Value,
    #[serde(default)]
    pub workspace: Value,
    pub team: Option<String>,
    pub tab_team: Option<String>,
    pub manager: Option<String>,
    #[serde(default)]
    pub managers: HashMap<String, Option<String>>,
}
/// A row that is a shell rather than a fleet agent: a utility kind, or a registration
/// the registry holds only so the session can be addressed (an MCP-opened tab, a
/// workspace's main shell) while it names no agent and none is detected running in it.
/// The negation of the frontend `isFleetAgent`.
pub(super) fn plain_shell(row: &Value) -> bool {
    match row["kind"].as_str() {
        Some("live-pty" | "root" | "notes" | "#") => true,
        Some("registered") => row["agentRevision"].is_null() && !row["detected"].is_object(),
        _ => false,
    }
}
impl TreeSession {
    fn handle(&self) -> &str {
        self.row["handle"].as_str().unwrap_or_default()
    }
    pub(super) fn team(&self) -> Option<&str> {
        self.team
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| self.story["teamKey"].as_str())
            .or_else(|| self.row["sessionId"].as_str().and(self.tab_team.as_deref()))
    }
    fn fleet(&self) -> bool {
        !plain_shell(&self.row)
    }
    fn manager(&self, other: Option<&str>) -> Option<&str> {
        match other {
            Some(team) => self.managers.get(team).and_then(|v| v.as_deref()),
            None => self.manager.as_deref(),
        }
    }
    fn launcher(&self, other: Option<&str>) -> Option<&str> {
        if let Some(chosen) = self.manager(other) {
            return (chosen != "@none").then_some(chosen);
        }
        if self.row["provenance"]["routineKey"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
        {
            return None;
        }
        self.row["launchedBy"].as_str()
    }
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TreeInput {
    pub team_key: String,
    #[serde(rename = "sessions")]
    pub tree_sessions: Vec<TreeSession>,
}

fn launch_tree(rows: Vec<&TreeSession>, other: Option<&str>) -> Vec<Value> {
    let launchers: HashSet<_> = rows
        .iter()
        .filter(|r| r.fleet())
        .filter_map(|r| r.launcher(other))
        .collect();
    let mut ordered: Vec<_> = rows
        .into_iter()
        .filter(|r| {
            r.fleet()
                || launchers.contains(r.handle())
                || r.row["sessionId"]
                    .as_str()
                    .is_some_and(|s| launchers.contains(s))
        })
        .collect();
    ordered.sort_by(|a, b| {
        b.row["startedAtMs"]
            .as_u64()
            .cmp(&a.row["startedAtMs"].as_u64())
            .then_with(|| a.handle().cmp(b.handle()))
    });
    let mut keys = HashMap::new();
    for row in &ordered {
        keys.insert(row.handle(), row.handle());
        if let Some(sid) = row.row["sessionId"].as_str() {
            keys.entry(sid).or_insert(row.handle());
        }
    }
    let mut parents = HashMap::new();
    for row in &ordered {
        if let Some(parent) = row
            .launcher(other)
            .and_then(|key| keys.get(key))
            .filter(|p| **p != row.handle())
        {
            parents.insert(row.handle(), *parent);
        }
    }
    for row in &ordered {
        let mut seen = HashSet::from([row.handle()]);
        let mut at = parents.get(row.handle()).copied();
        while let Some(key) = at {
            if !seen.insert(key) {
                break;
            }
            at = parents.get(key).copied();
        }
        if at == Some(row.handle()) {
            parents.remove(row.handle());
        }
    }
    fn node(
        row: &TreeSession,
        ordered: &[&TreeSession],
        parents: &HashMap<&str, &str>,
        other: Option<&str>,
    ) -> Value {
        let children: Vec<_> = ordered
            .iter()
            .filter(|r| parents.get(r.handle()) == Some(&row.handle()))
            .map(|r| node(r, ordered, parents, other))
            .collect();
        let mut n =
            json!({"row":row.row,"story":row.story,"workspace":row.workspace,"children":children});
        if parents.contains_key(row.handle()) {
            n["parentSource"] = if row.manager(other).is_some() {
                "chosen"
            } else {
                "launcher"
            }
            .into();
        }
        if let Some(team) = other {
            n["teamKey"] = team.into();
        }
        n
    }
    let roots: Vec<_> = ordered
        .iter()
        .filter(|r| !parents.contains_key(r.handle()))
        .map(|r| node(r, &ordered, &parents, other))
        .collect();
    roots
        .iter()
        .filter(|r| !r["children"].as_array().unwrap().is_empty())
        .chain(
            roots
                .iter()
                .filter(|r| r["children"].as_array().unwrap().is_empty()),
        )
        .cloned()
        .collect()
}
pub(super) fn build_tree(input: &TreeInput) -> Vec<Value> {
    launch_tree(
        input
            .tree_sessions
            .iter()
            .filter(|r| {
                r.row["alive"] == true && TreeSession::team(r).is_none_or(|t| t == input.team_key)
            })
            .collect(),
        None,
    )
}
pub(super) fn other_groups(input: &TreeInput) -> Vec<Value> {
    let mut teams = BTreeMap::<&str, Vec<&TreeSession>>::new();
    for row in &input.tree_sessions {
        if row.row["alive"] != true {
            continue;
        }
        if let Some(team) = row.team().filter(|t| *t != input.team_key) {
            teams.entry(team).or_default().push(row);
        }
    }
    let mut teams: Vec<_> = teams.into_iter().collect();
    teams.sort_by(|a, b| {
        a.0.rsplit(':')
            .next()
            .unwrap_or(a.0)
            .to_lowercase()
            .cmp(&b.0.rsplit(':').next().unwrap_or(b.0).to_lowercase())
            .then_with(|| a.0.cmp(b.0))
    });
    teams
        .into_iter()
        .filter_map(|(team, rows)| {
            let nodes = launch_tree(rows, Some(team));
            (!nodes.is_empty()).then(|| json!({"teamKey":team,"nodes":nodes}))
        })
        .collect()
}
pub(super) fn flatten(nodes: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for node in nodes {
        out.push(node.clone());
        out.extend(flatten(
            node["children"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default(),
        ));
    }
    out
}
pub(super) fn by_workspace(nodes: &[Value], order: &[Value]) -> Vec<Value> {
    let mut groups: Vec<Value> = Vec::new();
    for mut node in flatten(nodes) {
        let ws = node["workspace"].clone();
        let id = ws["id"].as_str().unwrap_or("");
        let index = groups.iter().position(|g| g["workspaceId"] == id).unwrap_or_else(|| {
            let mut group = json!({"workspaceId":id,"label":ws["label"].as_str().unwrap_or("No workspace"),"kind":ws["kind"].as_str().unwrap_or("none"),"nodes":[]});
            for key in ["storyStatus","storyTitle","sourceRepoSlug","machineId"] { if ws[key].as_str().is_some_and(|s| !s.is_empty()) { group[key] = ws[key].clone(); } }
            groups.push(group); groups.len()-1
        });
        node["children"] = json!([]);
        groups[index]["nodes"].as_array_mut().unwrap().push(node);
    }
    let no_ws = groups
        .iter()
        .position(|g| g["workspaceId"] == "")
        .map(|at| groups.remove(at));
    let mut known: Vec<_> = groups
        .iter()
        .filter(|g| order.contains(&g["workspaceId"]))
        .cloned()
        .collect();
    known.sort_by_key(|g| order.iter().position(|id| id == &g["workspaceId"]).unwrap());
    let mut known = known.into_iter();
    for g in &mut groups {
        if order.contains(&g["workspaceId"]) {
            *g = known.next().unwrap();
        }
    }
    if let Some(last) = no_ws {
        groups.push(last);
    }
    groups
}
#[cfg(test)]
pub(super) fn by_manager(nodes: &[Value], launchers: &Value) -> Vec<Value> {
    let flat = flatten(nodes);
    fn node(value: &Value, flat: &[Value], launchers: &Value) -> Value {
        let mut n = value.clone();
        n["children"] = flat
            .iter()
            .filter(|v| {
                launchers[v["row"]["handle"].as_str().unwrap_or_default()] == value["row"]["handle"]
            })
            .map(|v| node(v, flat, launchers))
            .collect();
        n
    }
    flat.iter()
        .filter(|v| {
            !flat.iter().any(|p| {
                p["row"]["handle"] == launchers[v["row"]["handle"].as_str().unwrap_or_default()]
            })
        })
        .map(|v| node(v, &flat, launchers))
        .collect()
}
