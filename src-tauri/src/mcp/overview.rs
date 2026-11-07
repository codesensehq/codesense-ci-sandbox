// SPDX-License-Identifier: MIT
//! `codesense_overview`: one read-only call that answers "what is going on in CodeSense
//! right now?" for an agent that has nothing but the CodeSense MCP, in any project.
//!
//! It reads the sources the drill-in tools already read (the session list, machines,
//! Agents, routines, the active team's inbox and role queues, the terminal hosts' open
//! workspaces, the placed docs) and summarizes each as one section. Every section names
//! in `detail` the tools that return its full data, so the agent knows where to go next.
//! A section whose source can't be read carries `error` instead of failing the call.
//!
//! With `guide: true` it returns the concepts guide instead: the agent-oriented "how
//! CodeSense works" page that also ships in the bundled CodeSense docs, compiled in so it
//! reaches remote agents too, whose machines don't hold the docs checkout.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The concepts guide, as it ships in the bundled docs.
pub(crate) const CONCEPTS_GUIDE: &str =
    include_str!("../../../codesense-docs/getting-started/content/concepts/README.md");

/// Where the concepts guide sits inside the placed docs checkout.
pub(crate) const CONCEPTS_GUIDE_PATH: &str = "getting-started/content/concepts/README.md";

/// How many rows a list section shows before it says `truncated`.
const MAX_ROWS: usize = 25;

pub(crate) const DESCRIPTION: &str = "Overview of CodeSense right now, in ONE read-only call: start here to answer any question about CodeSense state. Returns { generatedAt, context (active org/team), machines (connection status), sessions (running agents/terminals by state and machine, who launched whom, who needs you), agents (saved Agents), routines (enabled, last run, needs you), inbox (human unread; sessions with unread mail), roleQueues (per role in the active team: pending/claimed/accepted deliveries and live instances), workspaces (open workspaces and their tabs), docs (where the bundled CodeSense docs and the concepts guide are) }. Each section has `detail`: the tools to call for the full data. A section that could not be read has `error` instead of data; the rest still answer. Lists are capped (`truncated: true` when cut). Optional {guide: true}: return the concepts guide instead (how CodeSense works: orgs, teams, content layers, workspaces, machines, sessions, Agents, roles, inboxes, routines, notes, dashboards, each with its MCP tools).";

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OverviewParams {
    /// Return the concepts guide (how CodeSense works, with the tool for each concept)
    /// instead of the state overview. Default false.
    #[serde(default)]
    pub guide: bool,
}

/// One role's mailbox in a team's store: its deliveries by state and its live instances.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RoleQueue {
    pub role: String,
    pub pending: u32,
    pub claimed: u32,
    pub accepted: u32,
    pub acknowledged: u32,
    pub live_instances: u32,
}

/// Everything the overview is composed from, each source read independently.
#[derive(Debug, Clone)]
pub(crate) struct OverviewInputs {
    pub generated_at: String,
    /// The app's active (org, team).
    pub active: Option<(String, String)>,
    /// `list_sessions` rows.
    pub sessions: Result<Vec<Value>, String>,
    /// `list_machines` rows for the active team.
    pub machines: Result<Vec<Value>, String>,
    /// `agent_list` rows.
    pub agents: Result<Vec<Value>, String>,
    /// `routine_list` rows.
    pub routines: Result<Vec<Value>, String>,
    /// The human's unread mail in the active team's inbox.
    pub human_unread: Result<u32, String>,
    /// The active team's role queues.
    pub roles: Result<Vec<RoleQueue>, String>,
    /// The terminal hosts' layout snapshots (each `{serverName, workspaces:{key: layout}}`).
    pub layouts: Result<Vec<Value>, String>,
    /// The placed docs checkout, when it could be resolved.
    pub docs_root: Option<PathBuf>,
    pub docs_version: Result<crate::context_repo::codesense_docs::DocsVersion, String>,
}

fn detail(tools: &[&str]) -> Value {
    json!(tools)
}

/// A section that could not be read: its error and where to look instead.
fn failed(error: &str, tools: &[&str]) -> Value {
    json!({ "error": error, "detail": detail(tools) })
}

fn str_at<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

fn count_by(rows: &[Value], key: impl Fn(&Value) -> Option<String>) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    for row in rows {
        if let Some(k) = key(row) {
            *counts.entry(k).or_insert(0) += 1;
        }
    }
    counts
}

/// Keeps `rows` to [`MAX_ROWS`], returning them and whether any were cut.
fn capped(rows: Vec<Value>) -> (Vec<Value>, bool) {
    let truncated = rows.len() > MAX_ROWS;
    (rows.into_iter().take(MAX_ROWS).collect(), truncated)
}

/// Picks `keys` from `row`, dropping absent or null ones.
fn pick(row: &Value, keys: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for key in keys {
        if let Some(v) = row.get(*key).filter(|v| !v.is_null()) {
            out.insert((*key).to_string(), v.clone());
        }
    }
    Value::Object(out)
}

const CONTEXT_TOOLS: &[&str] = &["describe_setup", "list_orgs_teams", "set_active_team"];
const MACHINE_TOOLS: &[&str] = &["list_machines", "open_remote_tab"];
const SESSION_TOOLS: &[&str] = &[
    "who_am_i",
    "list_sessions",
    "list_runs",
    "read_run",
    "read_run_evidence",
    "read_run_work_product",
    "session_usage",
    "task_list",
    "task_read",
    "model_rates",
    "set_model_rate",
    "read_terminal_tab",
    "send_to_session",
    "get_viewer_state",
];
const AGENT_TOOLS: &[&str] = &["agent_list", "agent_get"];
const ROUTINE_TOOLS: &[&str] = &["routine_list", "routine_get", "routine_runs", "routine_run"];
const INBOX_TOOLS: &[&str] = &[
    "who_am_i",
    "inbox_list",
    "list_sessions",
    "read_messages",
    "list_agent_updates",
    "get_human_status",
];
const ROLE_TOOLS: &[&str] = &["role_state", "send_role_message", "register_role"];
const WORKSPACE_TOOLS: &[&str] = &[
    "get_viewer_state",
    "list_sessions",
    "list_worktrees",
    "open_workspace",
];
const DOCS_TOOLS: &[&str] = &[
    "codesense_overview",
    "query_context",
    "update_codesense_docs",
];

fn context_section(active: &Option<(String, String)>) -> Value {
    match active {
        Some((org, team)) => json!({ "org": org, "team": team, "detail": detail(CONTEXT_TOOLS) }),
        None => json!({
            "org": null,
            "team": null,
            "note": "No team is active in the app.",
            "detail": detail(CONTEXT_TOOLS),
        }),
    }
}

fn machines_section(machines: &Result<Vec<Value>, String>) -> Value {
    let rows = match machines {
        Ok(rows) => rows,
        Err(e) => return failed(e, MACHINE_TOOLS),
    };
    let state = |row: &Value| -> Option<String> {
        row.get("status")
            .and_then(|s| str_at(s, "state"))
            .map(str::to_string)
    };
    let items: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut item = pick(row, &["machineId", "label", "kind", "remoteMcpControl"]);
            if let (Some(obj), Some(s)) = (item.as_object_mut(), state(row)) {
                obj.insert("status".into(), Value::String(s));
            }
            item
        })
        .collect();
    let (items, truncated) = capped(items);
    json!({
        "total": rows.len(),
        "byStatus": count_by(rows, state),
        "items": items,
        "truncated": truncated,
        "detail": detail(MACHINE_TOOLS),
    })
}

fn sessions_section(sessions: &Result<Vec<Value>, String>) -> Value {
    let rows = match sessions {
        Ok(rows) => rows,
        Err(e) => return failed(e, SESSION_TOOLS),
    };
    let needs_you: Vec<Value> = rows
        .iter()
        .filter(|row| str_at(row, "state") == Some("needs-you"))
        .map(|row| pick(row, &["handle", "displayName", "machine"]))
        .collect();
    let items: Vec<Value> = rows
        .iter()
        .filter(|row| str_at(row, "state") != Some("exited"))
        .map(|row| {
            pick(
                row,
                &[
                    "handle",
                    "displayName",
                    "state",
                    "machine",
                    "workspace",
                    "story",
                    "role",
                    "launchedBy",
                    "unread",
                ],
            )
        })
        .collect();
    let (items, truncated) = capped(items);
    json!({
        "total": rows.len(),
        "byState": count_by(rows, |r| str_at(r, "state").map(str::to_string)),
        "byMachine": count_by(rows, |r| Some(str_at(r, "machine").unwrap_or("unknown").to_string())),
        "needsYou": needs_you,
        "notExited": items,
        "truncated": truncated,
        "detail": detail(SESSION_TOOLS),
    })
}

fn agents_section(agents: &Result<Vec<Value>, String>) -> Value {
    let rows = match agents {
        Ok(rows) => rows,
        Err(e) => return failed(e, AGENT_TOOLS),
    };
    let items: Vec<Value> = rows
        .iter()
        .filter(|row| str_at(row, "scope") != Some("builtin"))
        .map(|row| pick(row, &["id", "name", "runtime", "scope", "team"]))
        .collect();
    let (items, truncated) = capped(items);
    json!({
        "total": rows.len(),
        "byScope": count_by(rows, |r| str_at(r, "scope").map(str::to_string)),
        "saved": items,
        "truncated": truncated,
        "detail": detail(AGENT_TOOLS),
    })
}

fn routines_section(routines: &Result<Vec<Value>, String>) -> Value {
    let rows = match routines {
        Ok(rows) => rows,
        Err(e) => return failed(e, ROUTINE_TOOLS),
    };
    let flag = |row: &Value, key: &str| row.get(key).and_then(Value::as_bool) == Some(true);
    let last_status = |row: &Value| -> Option<String> {
        row.get("last_run")
            .and_then(|r| str_at(r, "status"))
            .map(str::to_string)
    };
    let items: Vec<Value> = rows
        .iter()
        .map(|row| {
            pick(
                row,
                &[
                    "id",
                    "name",
                    "team",
                    "enabled",
                    "runs_on",
                    "armed_here",
                    "schedule_in_words",
                    "next_run_at",
                    "last_run",
                    "needs_you",
                ],
            )
        })
        .collect();
    let (items, truncated) = capped(items);
    json!({
        "total": rows.len(),
        "enabled": rows.iter().filter(|r| flag(r, "enabled")).count(),
        "needsYou": rows.iter().filter(|r| flag(r, "needs_you")).count(),
        "lastRunByStatus": count_by(rows, last_status),
        "items": items,
        "truncated": truncated,
        "detail": detail(ROUTINE_TOOLS),
    })
}

fn inbox_section(
    human_unread: &Result<u32, String>,
    sessions: &Result<Vec<Value>, String>,
    active: &Option<(String, String)>,
) -> Value {
    let agents_with_unread: Value = match sessions {
        Ok(rows) => Value::Array(
            rows.iter()
                .filter(|row| row.get("unread").and_then(Value::as_u64).unwrap_or(0) > 0)
                .map(|row| {
                    pick(
                        row,
                        &["handle", "displayName", "unread", "oldestUnreadAt", "state"],
                    )
                })
                .collect(),
        ),
        Err(e) => json!({ "error": e }),
    };
    let human: Value = match human_unread {
        Ok(n) => json!(n),
        Err(e) => json!({ "error": e }),
    };
    json!({
        "team": active.as_ref().map(|(org, team)| format!("{org}:{team}")),
        "humanUnread": human,
        "agentsWithUnread": agents_with_unread,
        "note": "A supported CLI with inboxWaiter true in list_sessions receives new mail through its native inbox path, including before its first prompt. With inboxWaiter false, mail stays stored and readable; no automatic wake is promised. Hermes receives mid-turn mail as next-turn context at pre_llm_call. Codex inbox wake requires 0.159.3+ in the supported 0.x line.",
        "detail": detail(INBOX_TOOLS),
    })
}

fn roles_section(
    roles: &Result<Vec<RoleQueue>, String>,
    active: &Option<(String, String)>,
) -> Value {
    let rows = match roles {
        Ok(rows) => rows,
        Err(e) => return failed(e, ROLE_TOOLS),
    };
    let unclaimed: Vec<&str> = rows
        .iter()
        .filter(|r| r.pending > 0)
        .map(|r| r.role.as_str())
        .collect();
    json!({
        "team": active.as_ref().map(|(org, team)| format!("{org}:{team}")),
        "roles": rows,
        "withUnclaimedWork": unclaimed,
        "detail": detail(ROLE_TOOLS),
    })
}

fn workspaces_section(layouts: &Result<Vec<Value>, String>) -> Value {
    let snapshots = match layouts {
        Ok(rows) => rows,
        Err(e) => return failed(e, WORKSPACE_TOOLS),
    };
    let mut open = Vec::new();
    for snapshot in snapshots {
        let host = str_at(snapshot, "serverName").unwrap_or("");
        let Some(workspaces) = snapshot.get("workspaces").and_then(Value::as_object) else {
            continue;
        };
        for (key, layout) in workspaces {
            let tabs = layout
                .get("tabs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let live = tabs
                .iter()
                .filter(|t| t.get("state").and_then(|s| str_at(s, "kind")) == Some("live"))
                .count();
            let descriptor = layout.get("descriptor").cloned().unwrap_or(Value::Null);
            let mut item = pick(
                &descriptor,
                &[
                    "displayName",
                    "kind",
                    "storyId",
                    "repo",
                    "branch",
                    "worktreePath",
                ],
            );
            if let Some(obj) = item.as_object_mut() {
                obj.insert("key".into(), json!(key));
                obj.insert("host".into(), json!(host));
                obj.insert("tabs".into(), json!(tabs.len()));
                obj.insert("liveTabs".into(), json!(live));
            }
            open.push(item);
        }
    }
    let total = open.len();
    let (open, truncated) = capped(open);
    json!({
        "total": total,
        "open": open,
        "truncated": truncated,
        "detail": detail(WORKSPACE_TOOLS),
    })
}

fn docs_section(
    root: &Option<PathBuf>,
    version: &Result<crate::context_repo::codesense_docs::DocsVersion, String>,
) -> Value {
    let info = crate::context_repo::codesense_docs::bundle_info();
    let path = |rel: &str| {
        root.as_ref()
            .map(|r| r.join(rel).to_string_lossy().into_owned())
    };
    let mut section = json!({
        "org": info.slug,
        "team": info.team,
        "root": root.as_ref().map(|r| r.to_string_lossy().into_owned()),
        "readme": path(&info.readme),
        "conceptsGuide": path(CONCEPTS_GUIDE_PATH),
        "startHere": path("getting-started/content/START-HERE.md"),
        "howToRead": format!(
            "codesense_overview {{\"guide\": true}} returns the concepts guide (works from any machine). \
             query_context {{\"where\": {{\"orgs\": [\"{}\"]}}, \"text\": \"<words>\"}} finds any docs page; \
             its rows carry the file path on this Mac. Start with getting-started/content/START-HERE.md for task routing. \
             Filter routine docs with query_context {{\"where\":{{\"orgs\":[\"codesense/docs\"]}},\"filters\":[{{\"key\":\"cs:topic\",\"op\":\"eq\",\"value\":\"routines\"}}]}}.",
            info.slug
        ),
        "detail": detail(DOCS_TOOLS),
    });
    match version {
        Ok(version) => {
            if let Some(fields) = serde_json::to_value(version)
                .ok()
                .and_then(|v| v.as_object().cloned())
            {
                section.as_object_mut().unwrap().extend(fields);
            }
        }
        Err(error) => {
            section["state"] = json!("error");
            section["error"] = json!(error);
            section["action"] =
                json!("Resolve the docs error before trusting placed documentation.");
        }
    }
    section
}

/// The overview, composed from `inputs`. Pure: no I/O.
pub(crate) fn compose(inputs: &OverviewInputs) -> Value {
    json!({
        "generatedAt": inputs.generated_at,
        "context": context_section(&inputs.active),
        "machines": machines_section(&inputs.machines),
        "sessions": sessions_section(&inputs.sessions),
        "agents": agents_section(&inputs.agents),
        "routines": routines_section(&inputs.routines),
        "inbox": inbox_section(&inputs.human_unread, &inputs.sessions, &inputs.active),
        "roleQueues": roles_section(&inputs.roles, &inputs.active),
        "workspaces": workspaces_section(&inputs.layouts),
        "docs": docs_section(&inputs.docs_root, &inputs.docs_version),
    })
}

/// What `{guide: true}` returns.
pub(crate) fn guide(docs_root: &Option<PathBuf>) -> Value {
    json!({
        "guide": CONCEPTS_GUIDE,
        "path": docs_root
            .as_ref()
            .map(|r| r.join(CONCEPTS_GUIDE_PATH).to_string_lossy().into_owned()),
    })
}

/// Each role's deliveries by state and its live (unexpired) registrations, in one
/// team's store. Read-only. `now_secs` is epoch seconds, the unit registrations use.
pub(crate) fn role_queue_counts(
    store: &crate::weft::WeftStore,
    now_secs: i64,
) -> Result<Vec<RoleQueue>, String> {
    let conn = store.conn().map_err(|e| e.to_string())?;
    let mut roles: BTreeMap<String, RoleQueue> = BTreeMap::new();
    let mut stmt = conn
        .prepare("SELECT role, state, COUNT(*) FROM wv_role_delivery GROUP BY role, state")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (role, state, n) = row.map_err(|e| e.to_string())?;
        let q = role_entry(&mut roles, role);
        match state.as_str() {
            "pending" => q.pending += n,
            "claimed" => q.claimed += n,
            "accepted" => q.accepted += n,
            "acknowledged" => q.acknowledged += n,
            _ => {}
        }
    }
    let mut stmt = conn
        .prepare(
            "SELECT role, COUNT(*) FROM wv_role_registration WHERE expires_at > ?1 GROUP BY role",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([now_secs], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (role, n) = row.map_err(|e| e.to_string())?;
        role_entry(&mut roles, role).live_instances = n;
    }
    Ok(roles.into_values().collect())
}

fn role_entry(roles: &mut BTreeMap<String, RoleQueue>, role: String) -> &mut RoleQueue {
    roles.entry(role.clone()).or_insert_with(|| RoleQueue {
        role,
        ..RoleQueue::default()
    })
}

/// Reads every source the overview needs from the running app. Never fails: a source
/// that can't be read becomes its section's `error`.
pub(crate) async fn gather(
    app: &tauri::AppHandle,
    scope: crate::context_repo::caller_scope::CallerScope,
) -> OverviewInputs {
    use crate::commands::agent_inbox::{AppInboxHost, InboxHost as _};
    use crate::domain::Principal;
    use tauri::Manager;

    let active = app
        .try_state::<super::state::ActiveTeamState>()
        .and_then(|state| state.get())
        .map(|a| (a.org_slug, a.team_name));
    let ambient = crate::commands::route_to_context::resolve_ambient(app).await;

    let sessions = super::session_list::read_session_list(app, None)
        .await
        .and_then(|rows| to_rows(&rows));

    let machines = match &active {
        Some((org, team)) => crate::commands::machines::list_machines(
            app.clone(),
            org.clone(),
            Some(team.clone()),
            None,
        )
        .await
        .and_then(|rows| to_rows(&rows)),
        None => Err("no active team: machines are listed per team".to_string()),
    };

    let (agents, routines, docs_root) = match &ambient {
        Ok((home, user_id)) => {
            let principal = Principal::from(*user_id);
            let (home_a, principal_a) = (home.clone(), principal.clone());
            let agent_scope = scope.clone();
            let agents = crate::commands::await_blocking("mcp.overview.agents", move || {
                let params = serde_json::from_value(json!({})).map_err(|e| e.to_string())?;
                let reply =
                    super::agent_tools::list_for(&home_a, &principal_a, params, &agent_scope)?;
                envelope_rows(serde_json::to_value(reply).map_err(|e| e.to_string())?)
            })
            .await;
            let (home_r, principal_r) = (home.clone(), principal.clone());
            let routines = crate::commands::await_blocking("mcp.overview.routines", move || {
                let reply = super::routine_tools::list_for(
                    &home_r,
                    &principal_r,
                    Default::default(),
                    chrono::Utc::now(),
                    &scope,
                )?;
                envelope_rows(serde_json::to_value(reply).map_err(|e| e.to_string())?)
            })
            .await;
            let docs = crate::context_repo::codesense_docs::docs_root(home, &principal);
            (agents, routines, docs.is_dir().then_some(docs))
        }
        Err(e) => (Err(e.clone()), Err(e.clone()), None),
    };

    let (human_unread, roles) = match &active {
        Some((org, team)) => {
            let host = AppInboxHost { app: app.clone() };
            let team = crate::commands::dashboard_launch::LaunchTeam {
                org: org.clone(),
                team: team.clone(),
            };
            match host.team_store(&team).await {
                Ok(path) if !path.exists() => (Ok(0), Ok(Vec::new())),
                Ok(path) => {
                    let read = crate::commands::await_blocking("mcp.overview.inbox", move || {
                        let store = crate::weft::WeftStore::open(path) // RI-2-ALLOW
                            .map_err(|e| format!("open inbox store: {e}"))?;
                        let unread =
                            crate::weft::inbox::inbox_unread(&store, crate::weft::inbox::HUMAN)
                                .map(|u| u.count)
                                .map_err(|e| e.to_string());
                        let roles = role_queue_counts(&store, chrono::Utc::now().timestamp());
                        Ok((unread, roles))
                    })
                    .await;
                    match read {
                        Ok(pair) => pair,
                        Err(e) => (Err(e.clone()), Err(e)),
                    }
                }
                Err(e) => (Err(e.clone()), Err(e)),
            }
        }
        None => {
            let e = "no active team: the inbox and role queues are per team".to_string();
            (Err(e.clone()), Err(e))
        }
    };

    let layouts = match crate::commands::pty_layout::webview_layout_snapshots(app.clone()).await {
        Some(Ok(snapshots)) => to_rows(&snapshots),
        Some(Err(e)) => Err(e),
        None => Ok(Vec::new()),
    };

    let docs_version = match ambient {
        Ok((home, user_id)) => {
            let app = app.clone();
            crate::commands::await_blocking("mcp.overview.docs", move || {
                let archive = crate::context_repo::codesense_docs::bundled_archive(&app)?;
                crate::context_repo::codesense_docs::version(
                    &home,
                    &Principal::from(user_id),
                    archive,
                )
            })
            .await
        }
        Err(error) => Err(error),
    };

    OverviewInputs {
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        active,
        sessions,
        machines,
        agents,
        routines,
        human_unread,
        roles,
        layouts,
        docs_root,
        docs_version,
    }
}

/// The placed docs checkout for the ambient profile, when it exists.
pub(crate) async fn docs_root(app: &tauri::AppHandle) -> Option<PathBuf> {
    let (home, user_id) = crate::commands::route_to_context::resolve_ambient(app)
        .await
        .ok()?;
    let root = crate::context_repo::codesense_docs::docs_root(
        &home,
        &crate::domain::Principal::from(user_id),
    );
    root.is_dir().then_some(root)
}

fn to_rows<T: Serialize>(rows: &[T]) -> Result<Vec<Value>, String> {
    rows.iter()
        .map(|row| serde_json::to_value(row).map_err(|e| e.to_string()))
        .collect()
}

/// The rows of a tool envelope (`{status:"ok", data:[...]}` or `{ok:true, data:[...]}`),
/// or the envelope itself as the error when it carries no list.
fn envelope_rows(envelope: Value) -> Result<Vec<Value>, String> {
    match envelope.get("data").and_then(Value::as_array) {
        Some(rows) => Ok(rows.clone()),
        None => Err(envelope.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tab CodeSense does not manage (a hand-typed agent) is told apart by `inboxWaiter`
    /// false, and the overview says what that promises and what mail does meanwhile.
    #[test]
    fn the_inbox_note_says_what_an_unwoken_agent_gets() {
        let note = inbox_section(&Ok(0), &Ok(Vec::new()), &None)["note"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            note.contains(
                "With inboxWaiter false, mail stays stored and readable; no automatic wake is promised"
            ),
            "{note}"
        );
        assert!(note.contains("next-turn context"), "{note}");
    }

    /// Drill-in tools that ship separately from this one; every other name must be served.
    const PENDING_TOOLS: &[&str] = &["inbox_list", "role_state", "list_agent_updates"];

    const SECTIONS: &[&str] = &[
        "context",
        "machines",
        "sessions",
        "agents",
        "routines",
        "inbox",
        "roleQueues",
        "workspaces",
        "docs",
    ];

    fn empty() -> OverviewInputs {
        OverviewInputs {
            generated_at: "2026-10-04T00:00:00Z".into(),
            active: None,
            sessions: Ok(vec![]),
            machines: Ok(vec![]),
            agents: Ok(vec![]),
            routines: Ok(vec![]),
            human_unread: Ok(0),
            roles: Ok(vec![]),
            layouts: Ok(vec![]),
            docs_root: None,
            docs_version: Ok(crate::context_repo::codesense_docs::DocsVersion {
                bundled_commit: "bundle".into(),
                placed_commit: None,
                app_version: "test",
                state: "missing",
                action: "Call update_codesense_docs",
            }),
        }
    }

    #[test]
    fn unreadable_docs_report_error_without_losing_other_sections() {
        let mut inputs = empty();
        inputs.docs_version = Err("cannot read docs".into());
        let out = compose(&inputs);
        assert_eq!(out["docs"]["state"], "error");
        assert_eq!(out["docs"]["error"], "cannot read docs");
        assert!(out["sessions"].get("error").is_none());
        assert!(out["docs"].get("conceptsGuide").is_some());
    }

    #[test]
    fn docs_versions_are_collected_inputs_and_keep_links_and_detail() {
        let mut inputs = empty();
        inputs.docs_version = Ok(crate::context_repo::codesense_docs::DocsVersion {
            bundled_commit: "new".into(),
            placed_commit: Some("old".into()),
            app_version: "test",
            state: "behind-and-modified",
            action: "Call update_codesense_docs",
        });
        let docs = compose(&inputs)["docs"].clone();
        assert_eq!(docs["bundledCommit"], "new");
        assert_eq!(docs["placedCommit"], "old");
        assert_eq!(docs["appVersion"], "test");
        assert_eq!(docs["state"], "behind-and-modified");
        assert!(detail_of(&docs).contains(&"update_codesense_docs".into()));
        assert!(docs.get("readme").is_some());
        assert!(docs.get("howToRead").is_some());
    }

    fn populated() -> OverviewInputs {
        OverviewInputs {
            active: Some(("acme/ctx".into(), "web".into())),
            sessions: Ok(vec![
                json!({"handle":"lead","displayName":"lead","state":"running","machine":"This Mac","launchedBy":null,"unread":0,"oldestUnreadAt":null}),
                json!({"handle":"w1","displayName":"worker 1","state":"needs-you","machine":"box","launchedBy":"lead","unread":2,"oldestUnreadAt":"2026-10-04T00:00:00.000Z","role":"worker"}),
                json!({"handle":"w2","displayName":"worker 2","state":"idle","machine":"box","launchedBy":"lead","unread":null,"oldestUnreadAt":null}),
                json!({"handle":"old","displayName":"old","state":"exited","machine":"This Mac","launchedBy":null,"unread":0,"oldestUnreadAt":null}),
            ]),
            machines: Ok(vec![
                json!({"machineId":"local","label":"This Mac","kind":"local","status":{"state":"connected","workspaces":2},"remoteMcpControl":false,"attention":0}),
                json!({"machineId":"box","label":"box","kind":"ssh","status":{"state":"disconnected"},"remoteMcpControl":true,"attention":0}),
            ]),
            agents: Ok(vec![
                json!({"id":"builtin/claude","name":"Claude","runtime":"claude","scope":"builtin","team":""}),
                json!({"id":"triage","name":"Triage","runtime":"claude","scope":"team","team":"web"}),
            ]),
            routines: Ok(vec![
                json!({"id":"r1","name":"Nightly","team":"web","enabled":true,"runs_on":"one_mac","armed_here":true,"schedule_in_words":"daily at 02:00","next_run_at":"2026-10-05T02:00:00Z","last_run":{"status":"failed","reason":"exit 1","at":"x"},"needs_you":true}),
                json!({"id":"r2","name":"Manual","team":"web","enabled":false,"runs_on":"each_mac","armed_here":false,"schedule_in_words":"manual","next_run_at":null,"last_run":null,"needs_you":false}),
            ]),
            human_unread: Ok(3),
            roles: Ok(vec![
                RoleQueue {
                    role: "reviewer".into(),
                    pending: 2,
                    claimed: 1,
                    live_instances: 1,
                    ..RoleQueue::default()
                },
                RoleQueue {
                    role: "manager".into(),
                    acknowledged: 4,
                    live_instances: 1,
                    ..RoleQueue::default()
                },
            ]),
            layouts: Ok(vec![json!({
                "serverName": "This Mac",
                "workspaces": {
                    "acme/ctx:web|story:WEB-1": {
                        "descriptor": {"displayName":"WEB-1","kind":"story","storyId":"WEB-1","repo":"acme/web","branch":"feat/WEB-1","worktreePath":"/wt/WEB-1"},
                        "tabs": [
                            {"session":"s1","name":"claude","placement":"tab","state":{"kind":"live"}},
                            {"session":"s2","name":"shell","placement":"tab","state":{"kind":"exited","exitCode":0}}
                        ]
                    }
                }
            })]),
            docs_root: Some(PathBuf::from("/home/u/.codesense/users/1/codesense-docs")),
            ..empty()
        }
    }

    fn detail_of(section: &Value) -> Vec<String> {
        section["detail"]
            .as_array()
            .expect("every section names its drill-in tools")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn empty_state_has_every_section_with_zero_counts_and_no_errors() {
        let out = compose(&empty());
        assert_eq!(out["generatedAt"], "2026-10-04T00:00:00Z");
        for section in SECTIONS {
            let s = &out[*section];
            assert!(s.is_object(), "{section} missing");
            assert!(s.get("error").is_none(), "{section} errored: {s}");
            assert!(!detail_of(s).is_empty(), "{section} has no detail tools");
        }
        assert_eq!(out["context"]["org"], Value::Null);
        assert_eq!(out["sessions"]["total"], 0);
        assert_eq!(out["sessions"]["needsYou"], json!([]));
        assert_eq!(out["machines"]["total"], 0);
        assert_eq!(out["agents"]["total"], 0);
        assert_eq!(out["routines"]["total"], 0);
        assert_eq!(out["inbox"]["humanUnread"], 0);
        assert_eq!(out["inbox"]["agentsWithUnread"], json!([]));
        assert_eq!(out["roleQueues"]["roles"], json!([]));
        assert_eq!(out["workspaces"]["total"], 0);
        assert_eq!(out["docs"]["org"], "codesense/docs");
        assert_eq!(out["docs"]["root"], Value::Null);
    }

    #[test]
    fn populated_state_summarizes_each_source() {
        let out = compose(&populated());
        assert_eq!(out["context"]["org"], "acme/ctx");
        assert_eq!(out["context"]["team"], "web");

        let m = &out["machines"];
        assert_eq!(m["total"], 2);
        assert_eq!(m["byStatus"], json!({"connected":1,"disconnected":1}));
        assert_eq!(
            m["items"][1],
            json!({"machineId":"box","label":"box","kind":"ssh","remoteMcpControl":true,"status":"disconnected"})
        );

        let s = &out["sessions"];
        assert_eq!(s["total"], 4);
        assert_eq!(
            s["byState"],
            json!({"exited":1,"idle":1,"needs-you":1,"running":1})
        );
        assert_eq!(s["byMachine"], json!({"This Mac":2,"box":2}));
        assert_eq!(
            s["needsYou"],
            json!([{"handle":"w1","displayName":"worker 1","machine":"box"}])
        );
        let live: Vec<&str> = s["notExited"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["handle"].as_str().unwrap())
            .collect();
        assert_eq!(live, ["lead", "w1", "w2"]);
        assert_eq!(s["notExited"][1]["launchedBy"], "lead");

        let a = &out["agents"];
        assert_eq!(a["total"], 2);
        assert_eq!(a["byScope"], json!({"builtin":1,"team":1}));
        assert_eq!(
            a["saved"],
            json!([{"id":"triage","name":"Triage","runtime":"claude","scope":"team","team":"web"}])
        );

        let r = &out["routines"];
        assert_eq!(r["total"], 2);
        assert_eq!(r["enabled"], 1);
        assert_eq!(r["needsYou"], 1);
        assert_eq!(r["lastRunByStatus"], json!({"failed":1}));

        let i = &out["inbox"];
        assert_eq!(i["team"], "acme/ctx:web");
        assert_eq!(i["humanUnread"], 3);
        assert_eq!(
            i["agentsWithUnread"],
            json!([{"handle":"w1","displayName":"worker 1","unread":2,"oldestUnreadAt":"2026-10-04T00:00:00.000Z","state":"needs-you"}])
        );

        let q = &out["roleQueues"];
        assert_eq!(q["withUnclaimedWork"], json!(["reviewer"]));
        assert_eq!(q["roles"][0]["liveInstances"], 1);

        let w = &out["workspaces"];
        assert_eq!(w["total"], 1);
        assert_eq!(
            w["open"][0],
            json!({"displayName":"WEB-1","kind":"story","storyId":"WEB-1","repo":"acme/web","branch":"feat/WEB-1","worktreePath":"/wt/WEB-1","key":"acme/ctx:web|story:WEB-1","host":"This Mac","tabs":2,"liveTabs":1})
        );

        let d = &out["docs"];
        assert_eq!(
            d["conceptsGuide"],
            "/home/u/.codesense/users/1/codesense-docs/getting-started/content/concepts/README.md"
        );
        assert!(d["howToRead"].as_str().unwrap().contains("\"guide\": true"));
    }

    #[test]
    fn a_failed_source_reports_its_error_and_keeps_the_rest() {
        let mut inputs = populated();
        inputs.machines = Err("no active team".into());
        inputs.sessions = Err("registry down".into());
        inputs.roles = Err("store locked".into());
        let out = compose(&inputs);
        assert_eq!(out["machines"]["error"], "no active team");
        assert_eq!(
            detail_of(&out["machines"]),
            ["list_machines", "open_remote_tab"]
        );
        assert_eq!(out["sessions"]["error"], "registry down");
        assert_eq!(out["inbox"]["agentsWithUnread"]["error"], "registry down");
        assert_eq!(out["inbox"]["humanUnread"], 3);
        assert_eq!(out["roleQueues"]["error"], "store locked");
        assert_eq!(out["agents"]["total"], 2);
    }

    #[test]
    fn long_lists_are_capped_and_say_so() {
        let mut inputs = empty();
        inputs.agents = Ok((0..40)
            .map(|n| json!({"id": format!("a{n}"), "name": "x", "runtime": "claude", "scope": "team"}))
            .collect());
        let out = compose(&inputs);
        assert_eq!(out["agents"]["total"], 40);
        assert_eq!(out["agents"]["saved"].as_array().unwrap().len(), MAX_ROWS);
        assert_eq!(out["agents"]["truncated"], true);
    }

    /// Every tool a section points to is one the server serves (or one in `PENDING_TOOLS`).
    #[test]
    fn every_drill_in_tool_is_served() {
        let router = super::super::handler::CodesenseHandler::served_tool_router();
        let out = compose(&populated());
        for section in SECTIONS {
            for tool in detail_of(&out[*section]) {
                assert!(
                    router.get(&tool).is_some() || PENDING_TOOLS.contains(&tool.as_str()),
                    "{section} names unknown tool {tool}"
                );
            }
        }
        assert!(router.get("codesense_overview").is_some());
    }

    /// Both teaching surfaces use the same tool-name check, with explicit field exceptions.
    fn assert_document_tools(label: &str, document: &str, non_tools: &[&str]) -> usize {
        let router = super::super::handler::CodesenseHandler::served_tool_router();
        let mut checked = 0;
        for (i, span) in document.split('`').enumerate() {
            let word = span.trim();
            let is_snake = i % 2 == 1
                && word.contains('_')
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !is_snake || non_tools.contains(&word) {
                continue;
            }
            checked += 1;
            assert!(
                router.get(word).is_some() || PENDING_TOOLS.contains(&word),
                "{label} names `{word}`, which is not a served tool"
            );
        }
        checked
    }

    #[test]
    fn the_concepts_guide_names_only_real_tools() {
        const NOT_TOOLS: &[&str] = &[
            "story_id",
            "source_repo",
            "depends_on",
            "runs_on",
            "one_mac",
            "each_mac",
            "armed_here",
            "created_by",
            "no_work",
        ];
        assert!(
            assert_document_tools("concepts guide", CONCEPTS_GUIDE, NOT_TOOLS) > 40,
            "expected the guide to name many tools"
        );
        for heading in [
            "## Orgs, teams and context repos",
            "## Content layers",
            "## Stories, workspaces and worktrees",
            "## Machines and remote tabs",
            "## Sessions and handles",
            "## Agents",
            "## Messaging: conversations, role inboxes and progress notes",
            "## Routines",
            "## Dashboards and the `window.codesense` bridge",
        ] {
            assert!(CONCEPTS_GUIDE.contains(heading), "guide lacks {heading}");
        }
    }

    #[test]
    fn guide_and_task_mirror_link_to_shipped_pages() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../codesense-docs/getting-started/content");
        let mirror = include_str!("../../../codesense-docs/getting-started/content/START-HERE.md");
        let section = CONCEPTS_GUIDE
            .split("## Start here: by task\n")
            .nth(1)
            .unwrap()
            .split("\n## ")
            .next()
            .unwrap();
        let expected = format!("# Start here: by task\n{}\n", section.trim_end())
            .replace("](../", "](")
            .replace("](README.md#", "](concepts/README.md#");
        assert_eq!(mirror, expected);
        let links = regex::Regex::new(r"\]\(([^)]+)\)").unwrap();
        for (directory, document) in [
            (root.join("concepts"), CONCEPTS_GUIDE),
            (root.clone(), mirror),
        ] {
            for link in links.captures_iter(document) {
                let (page, fragment) = link[1]
                    .split_once('#')
                    .map(|(p, a)| (p, Some(a)))
                    .unwrap_or((&link[1], None));
                if page.contains("://") {
                    continue;
                }
                let path = directory.join(page);
                // Handbook pages resolve too: the bundled handbook is committed as Markdown.
                assert!(path.exists(), "missing guide page {}", path.display());
                if let Some(anchor) = fragment.filter(|_| page.ends_with(".html")) {
                    let html = std::fs::read_to_string(&path).unwrap();
                    assert!(
                        html.contains(&format!("id=\"{anchor}\""))
                            || html.contains(&format!("id='{anchor}'")),
                        "missing anchor {}#{anchor}",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn the_agent_design_skill_names_only_real_tools() {
        let skill = include_str!(
            "../../../codesense-docs/getting-started/content/examples/skills/agent-design/SKILL.md"
        );
        assert!(
            assert_document_tools("agent-design skill", skill, &["session_id"]) > 25,
            "expected the skill to name its tools"
        );
    }

    #[test]
    fn solo_founder_examples_use_the_native_agent_role_and_routine_models() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../codesense-docs/getting-started/content/examples/agents/solo-founder");
        for seat in ["planner", "builder", "reviewer"] {
            let yaml = std::fs::read_to_string(
                root.join(format!("solo-founder-{seat}")).join("agent.yaml"),
            )
            .unwrap();
            let agent: crate::agents::model::Agent = serde_yaml::from_str(&yaml).unwrap();
            agent.validate().unwrap();
            assert_eq!(agent.id, format!("solo-founder-{seat}"));
            let yaml =
                std::fs::read_to_string(root.join("roles").join(format!("{}.yaml", agent.id)))
                    .unwrap();
            let role: crate::queue_roles::RoleConfig = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(role.id, agent.id);
        }
        let yaml = std::fs::read_to_string(
            root.join("solo-founder-planner/routines/solo-founder-queue-wake/routine.yml"),
        )
        .unwrap();
        let mut routine: crate::routines::model::RoutineDef = serde_yaml::from_str(&yaml).unwrap();
        // A co-located definition gets its agent from the directory at discovery.
        routine.agent = crate::routines::model::AgentRef::from("solo-founder-planner");
        routine.validate().unwrap();
        assert!(crate::routines::role_pending::matches(&routine));
    }

    #[test]
    fn guide_mode_returns_the_guide_and_its_placed_path() {
        let out = guide(&Some(PathBuf::from("/d")));
        assert_eq!(out["guide"], CONCEPTS_GUIDE);
        assert_eq!(out["path"], "/d/getting-started/content/concepts/README.md");
        assert_eq!(guide(&None)["path"], Value::Null);
    }

    #[test]
    fn role_queue_counts_reads_deliveries_by_state_and_live_instances() {
        use crate::weft::role_inbox::{
            claim_role_messages, register_role, send_role_message_commit,
        };
        let td = tempfile::TempDir::new().unwrap();
        let store = crate::weft::WeftStore::open(td.path().join("weft.sqlite3")).unwrap();
        assert_eq!(role_queue_counts(&store, 1_000).unwrap(), vec![]);
        let now = 1_000_000;
        for text in ["a", "b", "c"] {
            send_role_message_commit(
                &store, "sender", "reviewer", text, None, None, None, "h", now, None, None, None,
            )
            .unwrap();
        }
        send_role_message_commit(
            &store, "sender", "builder", "d", None, None, None, "h", now, None, None, None,
        )
        .unwrap();
        let reg = register_role(&store, "reviewer", "inst-1", now).unwrap();
        claim_role_messages(&store, "reviewer", "inst-1", reg.generation, 1, now).unwrap();
        // An expired registration is not live.
        register_role(&store, "builder", "inst-2", now - 10_000).unwrap();

        let counts = role_queue_counts(&store, now).unwrap();
        assert_eq!(
            counts,
            vec![
                RoleQueue {
                    role: "builder".into(),
                    pending: 1,
                    ..RoleQueue::default()
                },
                RoleQueue {
                    role: "reviewer".into(),
                    pending: 2,
                    claimed: 1,
                    live_instances: 1,
                    ..RoleQueue::default()
                },
            ]
        );
    }
}
