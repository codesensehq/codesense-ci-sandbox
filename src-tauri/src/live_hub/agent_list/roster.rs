// SPDX-License-Identifier: MIT
use serde_json::{json, Value};
use std::collections::HashSet;

pub(crate) fn map_roster(wire: &Value) -> Value {
    let value = |key: &str, fallback: Value| {
        wire.get(key)
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(fallback)
    };
    let mut row = json!({
        "handle":wire["name"],"kind":value("kind","controller".into()),"sessionId":value("sessionId",Value::Null),
        "alive":value("alive",true.into()),"backgrounded":value("backgrounded",false.into()),
        "startedWith":value("startedWith",wire["currentTask"].clone()),"startedAtMs":value("startedAtMs",Value::Null),
        "launcher":value("launcher",wire["workspace"].clone()),"workspace":value("workspace",Value::Null),
        "story":value("story",Value::Null),"lastOutputMs":value("lastOutputMs",Value::Null)
    });
    for key in [
        "status",
        "provenance",
        "accountProvenance",
        "displayName",
        "agentRevision",
    ] {
        if let Some(v) = wire.get(key) {
            row[key] = v.clone();
        }
    }
    row
}
pub(crate) fn union(durable: &[Value], live: &[Value]) -> Vec<Value> {
    let mut out = durable.to_vec();
    for row in &mut out {
        if let Some(current) = live.iter().find(|v| v["handle"] == row["handle"]) {
            let identity: Vec<_> = [
                "workspace",
                "story",
                "launchedBy",
                "role",
                "avatar",
                "machineId",
                "launchTeamKey",
            ]
            .into_iter()
            .map(|key| {
                let value = current
                    .get(key)
                    .filter(|v| !v.is_null())
                    .cloned()
                    .or_else(|| matches!(key, "workspace" | "story").then(|| row[key].clone()))
                    .unwrap_or(Value::Null);
                (key, value)
            })
            .collect();
            let agrees = identity.iter().all(|(key, value)| {
                if matches!(*key, "machineId" | "launchTeamKey") {
                    row.get(*key) == Some(value)
                } else {
                    row.get(*key).unwrap_or(&Value::Null) == value
                }
            });
            if !agrees {
                for (key, value) in identity {
                    row[key] = value;
                }
            }
        }
    }
    out.extend(
        live.iter()
            .filter(|v| !durable.iter().any(|d| d["handle"] == v["handle"]))
            .cloned(),
    );
    out
}
fn tab(sid: &str, indexes: &[Value]) -> Option<Value> {
    for index in indexes {
        for ws in index.as_object().into_iter().flatten().map(|(_, v)| v) {
            for tabs in ws.as_object().into_iter().flatten().map(|(_, v)| v) {
                if let Some(tab) = tabs
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|tab| tab["sessionId"] == sid)
                {
                    return Some(tab.clone());
                }
            }
        }
    }
    None
}
fn hand_row(base: &Value, sid: &str, tab: Option<Value>, detected: &Value) -> Value {
    let mut row = json!({"handle":base["handle"].as_str().unwrap_or(sid),"sessionId":sid,"kind":"hand-started","alive":true,"status":"running","backgrounded":false,"detected":detected});
    for key in [
        "startedWith",
        "startedAtMs",
        "launcher",
        "lastOutputMs",
        "launchedBy",
        "role",
        "avatar",
        "agentRevision",
    ] {
        row[key] = base[key].clone();
    }
    row["displayName"] = base
        .get("displayName")
        .filter(|v| !v.is_null())
        .cloned()
        .or_else(|| {
            tab.and_then(|t| {
                t["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|s| s.into())
            })
        })
        .unwrap_or(Value::Null);
    row
}
pub(super) fn with_hand_started(sessions: &Value, indexes: &[Value], detected: &Value) -> Value {
    let Some(sessions) = sessions.as_array() else {
        return Value::Null;
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for row in sessions {
        let sid = row["sessionId"].as_str();
        if let Some(sid) = sid {
            seen.insert(sid);
        }
        let found = sid.and_then(|s| detected.get(s));
        let utility = matches!(
            row["kind"].as_str(),
            Some("live-pty" | "root" | "notes" | "#")
        );
        out.push(
            if row["alive"] == true && utility && found.is_some() && sid.is_some() {
                hand_row(
                    row,
                    sid.unwrap(),
                    tab(sid.unwrap(), indexes),
                    found.unwrap(),
                )
            } else {
                row.clone()
            },
        );
    }
    for (sid, found) in detected.as_object().into_iter().flatten() {
        if seen.contains(sid.as_str()) {
            continue;
        }
        if let Some(tab) = tab(sid, indexes).filter(|t| t["exit"].is_null() || t["exit"] == false) {
            out.push(hand_row(&Value::Null, sid, Some(tab), found));
        }
    }
    out.into()
}
