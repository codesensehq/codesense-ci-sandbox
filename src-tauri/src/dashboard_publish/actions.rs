// SPDX-License-Identifier: MIT
//! Declared, typed dashboard actions (SW-DP-25). No caller supplies executable text.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const MAX_PER_HOUR: u32 = 30;
pub const MAX_RUNNING: u32 = 4;
pub const MAX_TEMPLATE_CHARS: usize = 500;
pub const MAX_ARGS_BYTES: usize = 1024;
pub type Actions = BTreeMap<String, Action>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub verb: String,
    pub who: Who,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Value>,
    /// Each choice maps to fixed field values, never caller-supplied values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub choices: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default, rename = "match", skip_serializing_if = "BTreeMap::is_empty")]
    pub match_fields: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, String>,
    #[serde(default = "default_hour")]
    pub max_per_hour: u32,
    #[serde(default = "default_running")]
    pub max_running: u32,
}
fn default_hour() -> u32 {
    5
}
fn default_running() -> u32 {
    2
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Who {
    Owner,
    Viewers,
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && !id.contains("..")
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}
fn template_ok(text: &str) -> bool {
    let rest = text.replace("{id}", "");
    text.chars().count() <= MAX_TEMPLATE_CHARS && !rest.contains(['{', '}'])
}
fn fields_ok(fields: &BTreeMap<String, Value>) -> bool {
    fields.len() <= 16
        && fields.iter().all(|(k, v)| {
            valid_id(k)
                && !matches!(
                    k.to_ascii_lowercase().as_str(),
                    "id" | "story_id" | "path" | "body" | "content" | "relationships"
                )
                && (v.is_string() || v.is_number() || v.is_boolean() || v.is_null())
                && v.to_string().len() <= 1024
        })
}
pub fn validate(actions: &Actions) -> Result<(), String> {
    if actions.len() > 32 {
        return Err("at most 32 actions".into());
    }
    for (name, a) in actions {
        let fail = |why: &str| Err(format!("actions.{name}: {why}"));
        if matches!(name.as_str(), "ownerMessage" | "ownerReact" | "ownerMarkRead") || !valid_id(name) {
            return fail("invalid action name");
        }
        if !super::catalog::ACTIONS.iter().any(|v| v.name == a.verb) {
            return fail("unknown verb");
        }
        if !(1..=MAX_PER_HOUR).contains(&a.max_per_hour)
            || !(1..=MAX_RUNNING).contains(&a.max_running)
        {
            return fail("caps exceed K45 (30 per hour, 4 running) or are zero");
        }
        let fixed_ok = |f: &BTreeMap<String, Value>| {
            fields_ok(f)
                && f.iter().all(|(k, v)| {
                    crate::commands::frontmatter_fields::validate_field_name(k).is_ok()
                        && !v.as_str().is_some_and(|v| v.chars().any(char::is_control))
                })
        };
        if !fixed_ok(&a.fields)
            || !fields_ok(&a.match_fields)
            || a.choices.len() > 32
            || a.choices
                .iter()
                .any(|(k, v)| !valid_id(k) || v.is_empty() || !fixed_ok(v))
        {
            return fail("invalid fixed fields or choices");
        }
        if a.prompt.as_ref().is_some_and(|s| !template_ok(s))
            || a.vars.len() > 16
            || a.vars.iter().any(|(k, v)| !valid_id(k) || !template_ok(v))
        {
            return fail("templates allow only {id} and at most 500 characters");
        }
        if a.verb != "sendMessage" && !a.to.is_empty() {
            return fail("recipients require sendMessage");
        }
        match a.verb.as_str() {
            "sendMessage" if super::inbox::valid_declaration(a) => {}
            "setFields"
                if (!a.fields.is_empty() || !a.choices.is_empty())
                    && a.agent.is_none()
                    && a.prompt.is_none()
                    && a.mode.is_none()
                    && a.routine.is_none()
                    && a.vars.is_empty() => {}
            "launchAgent"
                if a.agent.as_ref().is_some_and(|s| !s.trim().is_empty())
                    && a.prompt.is_some()
                    && matches!(a.mode.as_deref(), Some("headless" | "visible-unfocused"))
                    && a.fields.is_empty()
                    && a.choices.is_empty()
                    && a.routine.is_none()
                    && a.vars.is_empty() => {}
            "runRoutine"
                if a.routine.as_ref().is_some_and(|s| !s.trim().is_empty())
                    && a.agent.is_none()
                    && a.prompt.is_none()
                    && a.mode.is_none()
                    && a.fields.is_empty()
                    && a.choices.is_empty() => {}
            _ => return fail("missing or unsupported parameters for verb"),
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Args {
    pub id: Option<String>,
    pub choice: Option<String>,
}
impl Action {
    pub fn args(&self, value: Value) -> Result<Args, &'static str> {
        if !value.is_object() || value.to_string().len() > MAX_ARGS_BYTES {
            return Err("invalid_args");
        }
        let args: Args =
            super::catalog::strict(value, &["id", "choice"]).map_err(|_| "invalid_args")?;
        let needs_id = self.verb != "runRoutine"
            || !self.match_fields.is_empty()
            || self.vars.values().any(|s| s.contains("{id}"));
        if (needs_id && args.id.is_none()) || args.id.as_ref().is_some_and(|s| !valid_id(s)) {
            return Err("invalid_args");
        }
        match &args.choice {
            Some(c) if self.verb == "setFields" && self.choices.contains_key(c) => {}
            None if self.choices.is_empty() => {}
            _ => return Err("invalid_args"),
        }
        Ok(args)
    }
}

/// Resolve a declaration's agent/routine through the same readers as the MCP tools.
fn source_caller(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    scope: Option<&super::state::Scope>,
) -> crate::context_repo::caller_scope::CallerScope {
    scope
        .map(|scope| {
            crate::context_repo::caller_scope::CallerScope::team(
                home,
                principal,
                &crate::commands::dashboard_launch::LaunchTeam {
                    org: scope.org.clone(),
                    team: scope.team.clone(),
                },
            )
        })
        .unwrap_or(crate::context_repo::caller_scope::CallerScope::Unknown)
}

pub async fn validate_references(
    app: &tauri::AppHandle,
    actions: &Actions,
    scope: Option<&super::state::Scope>,
) -> Result<(), String> {
    validate_references_for(app, actions, scope, None).await
}

pub(crate) async fn validate_references_for<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    actions: &Actions,
    scope: Option<&super::state::Scope>,
    caller: Option<crate::context_repo::caller_scope::CallerScope>,
) -> Result<(), String> {
    if actions
        .values()
        .all(|a| a.agent.is_none() && a.routine.is_none())
    {
        return Ok(());
    }
    let ctx = crate::commands::route_to_context::derive_active_context(app).await?;
    let home = ctx.codesense_home.clone();
    let principal = ctx.principal();
    let caller = caller.unwrap_or_else(|| source_caller(&home, &principal, scope));
    let actions = actions.clone();
    let scope = scope.cloned();
    crate::commands::await_blocking("dashboard.actions.validate", move || {
        for a in actions.values() {
            if let Some(agent) = &a.agent {
                let team = scope.as_ref().map(|s| (s.org.as_str(), s.team.as_str()));
                let key =
                    crate::agents::launch::find_agent_for(&home, &principal, agent, team, &caller)?;
                crate::agents::service::read_current(&home, &principal, &key)
                    .map_err(|e| e.to_string())?;
            }
            if let Some(routine) = &a.routine {
                let reply = crate::mcp::routine_tools::list_for(
                    &home,
                    &principal,
                    serde_json::from_value(serde_json::json!({})).map_err(|e| format!("{e}"))?,
                    chrono::Utc::now(),
                    &caller,
                )?;
                let value = serde_json::to_value(reply).map_err(|e| e.to_string())?;
                resolve_key(&value["data"], routine)?;
            }
        }
        Ok(())
    })
    .await
}
fn resolve_key(rows: &Value, key: &str) -> Result<String, String> {
    let rows = rows.as_array().ok_or("catalog unavailable")?;
    let names: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
    if names.contains(&key) {
        return Ok(key.to_string());
    }
    let matches: Vec<_> = names
        .into_iter()
        .filter(|s| s.rsplit('/').next() == Some(key))
        .collect();
    if matches.len() == 1 {
        Ok(matches[0].to_string())
    } else {
        Err("unknown or ambiguous agent/routine".into())
    }
}

pub async fn resolve_id(
    app: &tauri::AppHandle,
    folder: &std::path::Path,
    scope: Option<&super::state::Scope>,
    action: &Action,
    id: &str,
) -> Result<String, &'static str> {
    use tauri::Manager;
    if !valid_id(id) {
        return Err("invalid_args");
    }
    let service = app
        .try_state::<crate::context_query::ContextIndexService>()
        .ok_or("not_found")?
        .inner()
        .clone();
    let (org, team) = super::catalog::check_scope(
        scope,
        super::catalog::team_of(&service.snapshot().roots, folder),
    )
    .map_err(|_| "not_found")?;
    let query = id_query(&org, &team, action, id)?;
    let requested_id = id.to_string();
    let viewer = crate::context_query::Viewer {
        page: Some(folder.display().to_string()),
        ..Default::default()
    };
    crate::commands::await_blocking("dashboard.actions.resolve", move || {
        let result = crate::context_query::run_query(&service, &query, &viewer)
            .map_err(|_| "not_found".to_string())?;
        checked_path(
            &service.snapshot().roots,
            &org,
            &team,
            &requested_id,
            &result,
        )
        .map_err(String::from)
    })
    .await
    .map_err(|_| "not_found")
}

fn id_query(
    org: &str,
    team: &str,
    action: &Action,
    id: &str,
) -> Result<crate::context_query::ContextQuery, &'static str> {
    serde_json::from_value(serde_json::json!({"where":{"orgs":[org],"teams":[team],"layers":["team"]}, "filters":[{"key":"id","op":"eq","value":id}], "match":action.match_fields,"limit":2})).map_err(|_| "invalid_args")
}
fn checked_path(
    roots: &[crate::context_query::index::IndexRoot],
    org: &str,
    team: &str,
    requested_id: &str,
    result: &crate::context_query::ContextQueryResult,
) -> Result<String, &'static str> {
    if result.total != 1 || result.rows.len() != 1 {
        return Err("not_found");
    }
    let row = &result.rows[0];
    if row.id != requested_id {
        return Err("not_found");
    }
    let path = std::fs::canonicalize(&row.path).map_err(|_| "not_found")?;
    if super::catalog::team_of(roots, &path) != Some((org.into(), team.into())) {
        return Err("not_found");
    }
    if std::fs::symlink_metadata(&row.path)
        .map_err(|_| "not_found")?
        .file_type()
        .is_symlink()
    {
        return Err("not_found");
    }
    Ok(path.display().to_string())
}
pub(crate) fn fixed_fields(
    a: &Action,
    args: &Args,
) -> Result<BTreeMap<String, Value>, &'static str> {
    let mut fields = a.fields.clone();
    if let Some(choice) = &args.choice {
        fields.extend(a.choices.get(choice).ok_or("invalid_args")?.clone());
    }
    Ok(fields)
}

/// Uses the normal workspace port, including its gate and app concurrency cap.
pub(crate) async fn launch(
    port: &dyn crate::commands::workspace_open::OpenWorkspacePort,
    a: &Action,
    id: &str,
    scope: &super::state::Scope,
    slug: &str,
    has_worktree: bool,
    caller: (
        &std::path::Path,
        &crate::domain::Principal,
        &crate::context_repo::caller_scope::CallerScope,
    ),
) -> Result<Value, &'static str> {
    let mut params = serde_json::json!({"target": "story", "story_id": id, "team": scope.team, "agent": a.agent, "mode": a.mode, "initial_prompt": a.prompt.as_deref().unwrap_or("").replace("{id}", id), "role": format!("dash:{slug}"), "machine_id": crate::commands::machines::LOCAL_MACHINE_ID});
    // The current story spine can create worktrees. A dashboard instead uses the
    // documented team-workspace fallback when no linked worktree exists.
    if !has_worktree {
        params["target"] = "team-workspace".into();
        params["name"] = id.into();
        params.as_object_mut().unwrap().remove("story_id");
        params.as_object_mut().unwrap().remove("machine_id");
    }
    let result = crate::commands::workspace_open::open_workspace_call_for(
        port,
        serde_json::from_value(params).map_err(|_| "failed")?,
        Some(caller),
    )
    .await
    .map_err(|_| "failed")?;
    if result["status"] == "alreadyLive" || result["status"] == "busy" {
        return Err("busy");
    }
    if result["status"] != "opened" {
        return Err("failed");
    }
    result["handle"]
        .as_str()
        .map(|handle| serde_json::json!({"id": id, "handle": handle}))
        .ok_or("failed")
}

pub async fn execute(
    app: &tauri::AppHandle,
    folder: &std::path::Path,
    scope: Option<&super::state::Scope>,
    slug: &str,
    a: &Action,
    args: &Args,
) -> Result<Value, &'static str> {
    let ctx = crate::commands::route_to_context::derive_active_context(app)
        .await
        .map_err(|_| "failed")?;
    let caller = source_caller(&ctx.codesense_home, &ctx.principal(), scope);
    if a.verb != "runRoutine" {
        validate_references_for(
            app,
            &[("call".into(), a.clone())].into_iter().collect(),
            scope,
            Some(caller.clone()),
        )
        .await
        .map_err(|_| "not_found")?;
    }
    let path = match &args.id {
        Some(id) => Some(resolve_id(app, folder, scope, a, id).await?),
        None => None,
    };
    match a.verb.as_str() {
        "setFields" => {
            let fields = fixed_fields(a, args)?;
            // This is the app's current setField seam: byte-preserving typed writes,
            // including replacement of YAML lists and rejection of unsupported HTML keys.
            crate::commands::frontmatter_editor::update_frontmatter(
                app.clone(),
                path.ok_or("invalid_args")?,
                fields.clone().into_iter().collect(),
                None,
                None,
            )
            .await
            .map_err(|_| "failed")?;
            Ok(serde_json::json!({"id": args.id, "fields": fields}))
        }
        "launchAgent" => {
            let scope = scope.ok_or("not_found")?;
            let id = args.id.as_deref().ok_or("invalid_args")?;
            let records = crate::install::worktree_index::iter_story_records(
                &ctx.codesense_home,
                &ctx.principal(),
            )
            .map_err(|_| "failed")?;
            let resolved = std::path::PathBuf::from(path.ok_or("not_found")?);
            let has_worktree = records.iter().any(|r| {
                r.story_id == id
                    && r.org_slug == scope.org
                    && std::fs::canonicalize(&r.story_source_path).ok().as_ref() == Some(&resolved)
                    && r.code_repo_path.is_dir()
            });
            if has_worktree {
                // The normal spine resolves by id in the dashboard's org; refuse a story
                // of another team there.
                let owner = crate::commands::mcp_workspace::resolve_story_owner(
                    app,
                    id,
                    Some(scope.org.as_str()),
                )
                .await
                .map_err(|_| "not_found")?;
                if owner.org != scope.org || owner.team != scope.team {
                    return Err("not_found");
                }
            }
            launch(
                &ScopedPort {
                    app: app.clone(),
                    scope: scope.clone(),
                },
                a,
                id,
                scope,
                slug,
                has_worktree,
                (&ctx.codesense_home, &ctx.principal(), &caller),
            )
            .await
        }
        "runRoutine" => {
            let home = ctx.codesense_home.clone();
            let principal = ctx.principal();
            let app = app.clone();
            let routine = a.routine.clone().ok_or("not_found")?;
            let vars: BTreeMap<_, _> = a
                .vars
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.replace("{id}", args.id.as_deref().unwrap_or("")),
                    )
                })
                .collect();
            crate::commands::await_blocking("dashboard.actions.routine", move || {
                run_routine_for(
                    &home,
                    &principal,
                    &routine,
                    vars,
                    &|key, vars| {
                        crate::routines::machine_arm_app::fire_now(
                            &app, &home, &principal, key, vars,
                        )
                    },
                    &caller,
                )
            })
            .await
            .map_err(|_| "failed")
        }
        _ => Err("not_declared"),
    }
}

fn run_routine_for(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    reference: &str,
    vars: BTreeMap<String, String>,
    fire: crate::mcp::routine_tools::FireOnMachine<'_>,
    caller: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Value, String> {
    let list = crate::mcp::routine_tools::list_for(
        home,
        principal,
        serde_json::from_value(serde_json::json!({})).map_err(|e| e.to_string())?,
        chrono::Utc::now(),
        caller,
    )?;
    let key = resolve_key(
        &serde_json::to_value(list).map_err(|e| e.to_string())?["data"],
        reference,
    )?;
    // A machine-armed routine must be on the publishing Mac.
    let got = crate::mcp::routine_tools::get_for(
        home,
        principal,
        serde_json::from_value(serde_json::json!({"id": key})).map_err(|e| e.to_string())?,
        chrono::Utc::now(),
        caller,
    )?;
    let got = serde_json::to_value(got).map_err(|e| e.to_string())?;
    if got["data"]["routine"]["armed_machine"]
        .as_str()
        .is_some_and(|m| m != crate::commands::machines::LOCAL_MACHINE_ID)
    {
        return Err("routine is armed on another machine".into());
    }
    let params = serde_json::from_value(serde_json::json!({"id": key, "variables": vars}))
        .map_err(|e| e.to_string())?;
    let reply = crate::mcp::routine_tools::run_now_with_for(home, principal, params, fire, caller)?;
    let value = serde_json::to_value(reply).map_err(|e| e.to_string())?;
    if !value["data"]["run_id"].is_string() {
        return Err("failed".into());
    }
    Ok(serde_json::json!({"run_id": value["data"]["run_id"], "phase": value["data"]["outcome"]}))
}

pub const LOG: &str = "action-log.jsonl";
// All published action streams observe committed log writes, including actions
// which do not change a process session (for example, setFields).
fn log_changes() -> &'static tokio::sync::watch::Sender<()> {
    static CHANGES: std::sync::OnceLock<tokio::sync::watch::Sender<()>> =
        std::sync::OnceLock::new();
    CHANGES.get_or_init(|| tokio::sync::watch::channel(()).0)
}
pub(super) fn log_changed() {
    log_changes().send_replace(());
}

pub fn read_history(data_dir: &std::path::Path, slug: &str) -> Result<Vec<Value>, String> {
    Ok(read_all_history(data_dir)?
        .into_iter()
        .filter(|r| r["slug"] == slug)
        .collect())
}
/// Shared durable history for owner-wide inbox limits.
pub(crate) fn read_all_history(data_dir: &std::path::Path) -> Result<Vec<Value>, String> {
    let text = match std::fs::read_to_string(data_dir.join("dashboard-publish").join(LOG)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    let rows = text
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}
pub fn history(data_dir: &std::path::Path, slug: &str) -> Vec<Value> {
    read_history(data_dir, slug).unwrap_or_default()
}

pub fn authorize(by: &Value, owner: Option<&str>) -> Result<(), &'static str> {
    if by["role"] != "owner" || owner.is_none() || by["account"].as_str() != owner {
        Err("not_allowed")
    } else {
        Ok(())
    }
}
pub fn cap(
    a: &Action,
    name: &str,
    rows: &[Value],
    running: &[String],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), &'static str> {
    let calls: Vec<_> = rows
        .iter()
        .filter(|r| r["action"] == name && (r["outcome"] == "done" || r["outcome"] == "pending"))
        .collect();
    if calls
        .iter()
        .filter(|r| {
            r["at"]
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .is_some_and(|at| at > now - chrono::Duration::hours(1))
        })
        .count()
        >= a.max_per_hour as usize
    {
        return Err("rate_limited");
    }
    if a.verb == "launchAgent" {
        let handles: std::collections::HashSet<_> = calls
            .iter()
            .filter_map(|r| r["handle"].as_str())
            .filter(|h| running.iter().any(|r| r == h))
            .collect();
        let uncertain = calls.iter().filter(|r| r["outcome"] == "pending").count();
        if handles.len() + uncertain >= a.max_running as usize {
            return Err("busy");
        }
    }
    Ok(())
}

/// The `actions` stream's value, redacted like an answer.
fn stream_value(rows: Vec<Value>) -> Value {
    super::catalog::redact(serde_json::json!({ "actions": rows }))
}

/// P17 activity uses the existing value-stream channel; no addresses leave this Mac.
pub fn open_stream(
    ctx: super::catalog::LiveCtx,
    call: super::catalog::StreamCall,
) -> futures_util::future::BoxFuture<
    'static,
    Result<super::catalog::Items, super::catalog::QueryError>,
> {
    Box::pin(async move {
        use futures_util::StreamExt;
        use tauri::Manager;
        let _: BTreeMap<String, Value> = super::catalog::strict(call.args.clone(), &[])?;
        let app = ctx
            .app
            .ok_or_else(|| super::catalog::QueryError::Refused("failed"))?;
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|_| super::catalog::QueryError::Refused("failed"))?;
        let slug = super::manifest::load_slug(&call.folder)
            .map_err(|_| super::catalog::QueryError::Refused("not_found"))?;
        let changes = app.state::<crate::live_hub::LiveHub>().changes();
        let logged = log_changes().subscribe();
        let stream = futures_util::stream::unfold((app, call, data_dir, slug, None::<Value>, true, changes, logged), |(app, call, data_dir, slug, mut sent, mut first, mut changes, mut logged)| async move {
            loop {
                if !first { tokio::select! { _ = changes.changed() => {}, _ = logged.changed() => {}, _ = call.snap.notified() => sent = None } }
                first = false;
                let sessions = crate::mcp::session_list::read_session_list(&app, None).await.ok()?;
                let mut rows: Vec<Value> = history(&data_dir, &slug).into_iter().rev().take(50).collect();
                rows.reverse();
                let ctx = crate::commands::route_to_context::derive_active_context(&app).await.ok()?;
                let run_ids: Vec<String> = rows.iter().filter_map(|r| r["run_id"].as_str().map(String::from)).collect();
                let phases = crate::commands::await_blocking("dashboard.actions.phases", move || {
                    let mut phases = BTreeMap::new();
                    for id in run_ids {
                        if let Ok(reply) = crate::mcp::routine_tools::run(&ctx.codesense_home, &ctx.principal(), serde_json::from_value(serde_json::json!({"runId": id})).unwrap()) {
                            if let Ok(v) = serde_json::to_value(reply) { if v["data"]["phase"].is_string() { phases.insert(id, v["data"]["phase"].clone()); } }
                        }
                    }
                    Ok(phases)
                }).await.unwrap_or_default();
                let rows: Vec<_> = rows.iter().map(|r| {
                    let mut out = serde_json::json!({"at": r["at"], "action": r["action"], "id": r["args"]["id"], "by": {"role": r["by"]["role"]}, "outcome": r["outcome"], "state": if r["outcome"] == "done" { "done" } else if r["outcome"] == "pending" { "pending" } else { "refused" }});
                    if let Some(share) = r["by"].get("share_id") { out["by"]["share"] = share.clone(); }
                    if let Some(handle) = r["handle"].as_str() {
                        out["handle"] = handle.into();
                        out["state"] = sessions.iter().find(|s| s.handle == handle).map(|s| serde_json::json!(s.state)).unwrap_or_else(|| "exited".into());
                    }
                    if let Some(run) = r.get("run_id") { out["run_id"] = run.clone(); if let Some(phase) = phases.get(run.as_str().unwrap_or("")) { out["state"] = phase.clone(); } }
                    out
                }).collect();
                let value = stream_value(rows);
                if sent.as_ref() != Some(&value) {
                    sent = Some(value.clone());
                    return Some((super::catalog::StreamItem::Value(value), (app, call, data_dir, slug, sent, first, changes, logged)));
                }
            }
        }).boxed();
        Ok(stream)
    })
}

struct ScopedPort {
    app: tauri::AppHandle,
    scope: super::state::Scope,
}
#[async_trait::async_trait]
impl crate::commands::workspace_open::OpenWorkspacePort for ScopedPort {
    async fn launch_story(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: crate::commands::mcp_workspace::SessionView,
    ) -> Result<crate::commands::mcp_workspace::LaunchOutcome, String> {
        Err("target required".into())
    }
    async fn targeted(
        &self,
    ) -> Result<
        (
            crate::commands::workspace_open::ActiveTeam,
            Box<dyn crate::commands::workspace_open::OpenHost>,
        ),
        String,
    > {
        let ctx = crate::commands::route_to_context::derive_active_context(&self.app).await?;
        Ok((
            crate::commands::workspace_open::ActiveTeam {
                org: self.scope.org.clone(),
                team: self.scope.team.clone(),
            },
            Box::new(crate::commands::workspace_open::AppOpenHost {
                app: self.app.clone(),
                principal: ctx.principal(),
                codesense_home: ctx.codesense_home,
            }),
        ))
    }
    fn reveal(&self, handle: &str, focus: bool) {
        crate::commands::workspace_open::AppOpenWorkspacePort {
            app: self.app.clone(),
        }
        .reveal(handle, focus);
    }
}

#[cfg(test)]
mod tests;
