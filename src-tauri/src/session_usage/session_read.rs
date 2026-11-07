// SPDX-License-Identifier: MIT
//! One session's joined facts, shared by MCP and dashboard pages.
use super::{event::Tokens, Filter, StoryResolver, UsageStore};
use crate::context_repo::caller_scope::CallerScope;
use rmcp::schemars::JsonSchema;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

pub(crate) const UNKNOWN_SESSION: &str = "unknown session";

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Params {
    /// A CodeSense session id or the CLI's native session id. Names the subject, not the caller.
    pub session_id: String,
    /// Omit for facts only. `conversation` pages the stored turns in full. `turns` pages them as
    /// summaries (index, role, tsMs, text up to 200 chars, toolCalls, toolErrors, model) with no
    /// tool input or result and no provided skills (a call whose failure is not recorded adds
    /// toolErrorsUnknown), and the answer holds only the session's id, cli, branch and timing, its
    /// story, the reasons for those and the page. Not both.
    #[serde(default)]
    pub include: Vec<Include>,
    /// Next turn index from the preceding conversation page.
    pub cursor: Option<u32>,
    /// Turns to return (1–200). Defaults to 20, or 1 for a jump.
    pub limit: Option<usize>,
    /// Jump to this zero-based turn index; exclusive with cursor and toolUseId.
    pub turn: Option<u32>,
    /// Jump to the turn containing this CLI-native tool call id.
    pub tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Include {
    Conversation,
    Turns,
}

/// The longest turn text a summary keeps, in JSON bytes once escaped (so also in chars, since a
/// char is at least one byte): a page of 45 turns of any text (non-ASCII, quotes, control
/// characters) stays under 16 KB.
const SUMMARY_BYTES: usize = 200;

/// The start of `text` that is at most [`SUMMARY_BYTES`] bytes once JSON-escaped.
fn summary_text(text: &str) -> String {
    let mut out = String::with_capacity(SUMMARY_BYTES);
    let mut bytes = 0;
    for c in str::chars(text) {
        // What `serde_json` writes for the char inside a string.
        bytes += match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            c if c < ' ' => 6,
            c => c.len_utf8(),
        };
        if bytes > SUMMARY_BYTES {
            break;
        }
        out.push(c);
    }
    out
}

impl Params {
    fn selector(&self) -> Result<Option<crate::session_conversation::Selector>, String> {
        use crate::session_conversation::Selector;
        let count = usize::from(self.cursor.is_some())
            + usize::from(self.turn.is_some())
            + usize::from(self.tool_use_id.is_some());
        if count > 1 {
            return Err("invalid: choose only one of cursor, turn or toolUseId".into());
        }
        if self.limit.is_some_and(|n| n == 0 || n > 200) {
            return Err("invalid: limit must be 1–200".into());
        }
        if self
            .tool_use_id
            .as_ref()
            .is_some_and(|s| str::is_empty(s.trim()))
        {
            return Err("invalid: toolUseId must not be empty".into());
        }
        let (conversation, turns) = (
            self.include.contains(&Include::Conversation),
            self.include.contains(&Include::Turns),
        );
        if conversation && turns {
            return Err("invalid: include conversation or turns, not both".into());
        }
        if !conversation && !turns {
            if count > 0 || self.limit.is_some() {
                return Err(
                    "invalid: conversation selectors require include: [conversation] or [turns]".into(),
                );
            }
            return Ok(None);
        }
        Ok(Some(if let Some(index) = self.turn {
            Selector::Turn {
                index,
                limit: self.limit,
            }
        } else if let Some(id) = &self.tool_use_id {
            Selector::ToolUse {
                id: id.clone(),
                limit: self.limit,
            }
        } else {
            Selector::Page {
                cursor: self.cursor,
                limit: self.limit,
            }
        }))
    }
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Model {
    model: String,
    calls: u64,
    tokens: Tokens,
    cost_usd: Option<f64>,
    cost_source: Option<String>,
    reason: Option<String>,
    #[serde(skip)]
    sources: BTreeSet<String>,
    #[serde(skip)]
    unpriced: bool,
}

fn source(sources: &BTreeSet<String>) -> Option<String> {
    match sources.len() {
        0 => None,
        1 => sources.first().cloned(),
        _ => Some("mixed".into()),
    }
}

/// The session ids a launch's usage was exported under, besides its own.
pub(super) const LAUNCH_SESSIONS_SQL: &str =
    "SELECT ?1 UNION SELECT session_id FROM session_attribution WHERE launch_id = ?1";

impl UsageStore {
    /// Every event id-space under a launch, including usage written under the launch id itself.
    pub(super) fn event_session_ids(&self, session: &str) -> Result<Vec<String>, String> {
        let identity = resolve_session(self, None, session)?;
        let mut stmt = rusqlite::Connection::prepare(
            &self.conn,
            LAUNCH_SESSIONS_SQL,
        )
        .map_err(super::db_err)?;
        let rows = stmt
            .query_map(params![identity.session_id], |r| r.get(0))
            .map_err(super::db_err)?;
        let mut ids = rows
            .collect::<Result<Vec<String>, _>>()
            .map_err(super::db_err)?;
        ids.push(session.to_owned());
        ids.push(identity.native_session_id);
        if let Some(native) = identity.record.and_then(|r| r.native_session_id) {
            ids.push(native);
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
}

/// Read from these stores off the async runtime, then join the existing durable run reader.
/// Runtime facts are machine-wide; only story-file enrichment uses the caller's content scope.
pub async fn read(
    store: &Path,
    dir: &Path,
    params: &Params,
    scope: &CallerScope,
) -> Result<Value, String> {
    if str::is_empty(params.session_id.trim()) {
        return Err("invalid: sessionId must not be empty".into());
    }
    params.selector()?;
    count_read(store);
    let (store, dir, params, scope) = (
        store.to_owned(),
        dir.to_owned(),
        params.clone(),
        scope.clone(),
    );
    let mut result = crate::commands::await_blocking("session_read", move || {
        facts(&store, &dir, &params, &scope)
    })
    .await?;
    if let Some(id) = serde_json::Value::as_str(&result["session"]["runId"]) {
        let outcome =
            crate::commands::sprint_run::run_store::read_run_outcome(id.to_owned()).await?;
        result["run"] = match outcome {
            Some(outcome) => json!({"state":"present", "outcome":outcome}),
            None => json!({"state":"absent", "reason":"no outcome recorded for this run yet"}),
        };
    }
    Ok(result)
}

/// Session reads of each store, by its path (tests only).
#[cfg(test)]
static READS: std::sync::Mutex<BTreeMap<std::path::PathBuf, usize>> =
    std::sync::Mutex::new(BTreeMap::new());

/// How many session reads the store at `store` has answered (tests only).
#[cfg(test)]
pub(crate) fn reads(store: &Path) -> usize {
    READS.lock().unwrap().get(store).copied().unwrap_or(0)
}

#[cfg(test)]
fn count_read(store: &Path) {
    *READS.lock().unwrap().entry(store.to_owned()).or_default() += 1;
}
#[cfg(not(test))]
fn count_read(_store: &Path) {}

/// The current usage and conversation paths, shared by the two transport adapters.
pub async fn current(params: &Params, scope: &CallerScope) -> Result<Value, String> {
    let store = super::store_path()?;
    let dir =
        crate::session_conversation::conversations_dir().ok_or("no CodeSense home directory")?;
    read(&store, &dir, params, scope).await
}

pub(crate) struct SessionIdentity {
    pub session_id: String,
    pub native_session_id: String,
    pub record: Option<super::record::SessionRecord>,
}

/// One identity join for notes, task links, usage facts and checkpoint turn keys.
pub(crate) fn resolve_session(
    store: &UsageStore,
    cli: Option<&str>,
    input: &str,
) -> Result<SessionIdentity, String> {
    let observed = if input.starts_with("s_") {
        store
            .checkpoint_native_session(cli, input)?
            .unwrap_or_else(|| input.to_owned())
    } else {
        input.to_owned()
    };
    let mut record = store.read_session_record(&observed)?;
    let mut attributed_launch = None;
    if record.is_none() {
        let launch: Option<String> = store
            .conn
            .query_row(
                "SELECT launch_id FROM session_attribution WHERE session_id = ?1",
                [&observed],
                |r| r.get(0),
            )
            .optional()
            .map_err(super::db_err)?
            .flatten();
        if let Some(launch) = launch {
            record = store.read_session_record(&launch)?;
            attributed_launch = Some(launch);
        }
    }
    let session = record
        .as_ref()
        .map(|r| std::string::String::as_str(&r.session_id))
        .or(attributed_launch.as_deref())
        .unwrap_or(&observed)
        .to_owned();
    let native_session_id = if observed == session {
        record
            .as_ref()
            .and_then(|r| r.native_session_id.clone())
            .unwrap_or(observed)
    } else {
        observed
    };
    Ok(SessionIdentity {
        record,
        session_id: session,
        native_session_id,
    })
}

fn facts(path: &Path, dir: &Path, params: &Params, scope: &CallerScope) -> Result<Value, String> {
    let store = UsageStore::open(path)?;
    let input = params.session_id.trim();
    let identity = resolve_session(&store, None, input)?;
    let record = identity.record;
    let session = identity.session_id.as_str();
    let ids = store.event_session_ids(session)?;
    let ids_json = serde_json::to_string(&ids).map_err(|e| e.to_string())?;
    let mut stmt = rusqlite::Connection::prepare(
        &store.conn,
        super::cost_rollup::SESSION_USAGE_SQL,
    )
    .map_err(super::db_err)?;
    let rows = stmt
        .query_map([ids_json], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Tokens {
                    input: r.get(1)?,
                    output: r.get(2)?,
                    cache_read: r.get(3)?,
                    cache_write: r.get(4)?,
                    cache_write_1h: r.get(5)?,
                },
                r.get::<_, Option<f64>>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(super::db_err)?;
    let mut models: BTreeMap<String, Model> = BTreeMap::new();
    let mut rollup = super::cost_rollup::CostRollup::new();
    for row in rows {
        let (name, tokens, cost, from) = row.map_err(super::db_err)?;
        super::cost_rollup::CostRollup::add(&mut rollup, &name, cost);
        let m = models.entry(name.clone()).or_insert_with(|| Model {
            model: name,
            ..Default::default()
        });
        m.calls += 1;
        m.tokens.add(&tokens);
        if let Some(cost) = cost {
            *m.cost_usd.get_or_insert(0.0) += cost;
            if let Some(from) = from.as_deref().and_then(|s| match s {
                "reported" => Some("reported"),
                "rate" => Some("rate_table"),
                _ => None,
            }) {
                m.sources.insert(from.into());
            }
        } else {
            m.unpriced = true;
        }
    }
    if record.is_none() && std::collections::BTreeMap::is_empty(&models) {
        return Err(UNKNOWN_SESSION.into());
    }
    let mut unpriced = Vec::new();
    let mut sources = BTreeSet::new();
    for m in models.values_mut() {
        sources.extend(m.sources.iter().cloned());
        m.cost_source = source(&m.sources);
        if m.unpriced {
            unpriced.push(m.model.clone());
            m.cost_usd = None;
            m.reason = Some(format!("unpriced model: {}", m.model));
        }
    }
    let reason = if std::collections::BTreeMap::is_empty(&models) {
        Some("no usage events for this session".to_string())
    } else if !std::vec::Vec::is_empty(&unpriced) {
        Some(format!("unpriced models: {}", unpriced.join(", ")))
    } else {
        None
    };
    let mut launch_stories = HashMap::new();
    if let Some(r) = &record {
        if let (Some(handle), Some(story)) = (&r.handle, &r.story_id) {
            launch_stories.insert(handle.clone(), story.clone());
        }
    }
    let mut resolver =
        StoryResolver::for_home(path.parent().unwrap_or(Path::new(".")), launch_stories)
            .with_scope(scope.clone());
    let report = store.report_with(
        &Filter {
            session_id: Some(session.to_owned()),
            ..Default::default()
        },
        &mut resolver,
    )?;
    let story = report
        .sessions
        .iter()
        .find_map(|s| s.story.clone())
        .or_else(|| {
            record.as_ref().and_then(|r| {
                r.story_id.as_deref().map(|id| {
                    let source = r
                        .handle
                        .as_deref()
                        .and_then(|h| resolver.launch_story(h, r.org.as_deref()))
                        .and_then(|(_, p)| p);
                    resolver.story(id, source.as_deref())
                })
            })
        });
    // The durable metadata describes the tool index even after conversation text expires.
    use super::conversation::State;
    let conversation_meta = store.read_conversation_meta(session)?;
    let dropped_turns = conversation_meta.as_ref().and_then(|meta| meta.truncated.as_ref()).and_then(|truncation| truncation.dropped_turns);
    let (used_state, used_reason) = match conversation_meta.as_ref() {
        Some(meta) => match meta.state {
            State::Unavailable => (
                "unavailable",
                Some(meta.unavailable.get("conversation").cloned()
                    .unwrap_or_else(|| "capture could not obtain its session record".to_owned())),
            ),
            State::Present => ("present", None),
            State::Truncated => (
                "truncated",
                Some("the conversation was truncated; counts cover its retained tool-call index".to_owned()),
            ),
            State::Expired => (
                "expired",
                Some("the conversation passed its retention; its tool-call index remains".to_owned()),
            ),
            State::Unsupported => (
                "unsupported",
                Some(meta.unavailable.get("conversation").cloned()
                    .unwrap_or_else(|| "this CLI's conversation is not read".to_owned())),
            ),
        },
        None => match record.as_ref()
            .and_then(|r| r.cli.as_deref())
            .and_then(|cli| std::result::Result::ok(cli.parse::<codesense_pty_core::agent_cli::AgentCli>()))
            .and_then(crate::session_conversation::unsupported_reason)
        {
            Some(reason) => ("unsupported", Some(reason)),
            None => ("absent", Some("no conversation has been stored for this session yet".to_owned())),
        },
    };
    let mut tools: BTreeMap<(String, Option<String>), (u64, u64, u64, Option<Value>, Option<Value>)> = BTreeMap::new();
    for call in store.read_tool_calls(session)? {
        let entry = tools.entry((call.name, call.mcp_server)).or_default();
        entry.0 += 1;
        entry.1 += u64::from(call.is_error == Some(true));
        entry.2 += u64::from(call.is_error.is_none());
        if call.is_error == Some(true) && entry.3.is_none() {
            entry.3 = Some(json!({"turn":call.turn_index,"toolUseId":call.native_id}));
        }
        if call.is_error == Some(true) && entry.4.is_none()
            && matches!(used_state, "present" | "truncated")
            && !dropped_turns.is_some_and(|(first, last)| (first..=last).contains(&call.turn_index))
        {
            entry.4 = Some(json!({"turn":call.turn_index,"toolUseId":call.native_id}));
        }
    }
    let used:Vec<Value>=tools.into_iter().map(|((name,server),(calls,errors,unknown,first_error,first_retained_error))|
        json!({"name":name,"mcpServer":server,"calls":calls,"errors":errors,"errorUnknown":unknown,"firstError":first_error,"firstRetainedError":first_retained_error})).collect();
    let used = match used_state {
        "absent" | "unsupported" | "unavailable" => None,
        _ => Some(used),
    };
    let mut unavailable = BTreeMap::new();
    if story.is_none() {
        unavailable.insert("story", "unlinked: no story linked to this session");
    }
    if record.as_ref().is_none_or(|r| r.timing.is_none()) {
        unavailable.insert("timing", "absent: no timing recorded for this session");
    }
    if record.is_none() {
        unavailable.insert("toolsAllowed", "absent: no session record of allowed tools");
    }
    unavailable.insert(
        "retries",
        "not recorded: no retry signal in the session timeline",
    );
    if record.is_none() {
        unavailable.insert(
            "session",
            "no session record (hand-started or older session)",
        );
    }
    let direct_usage: bool = store
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM usage_event WHERE session_id = ?1)",
            [session],
            |r| r.get(0),
        )
        .map_err(super::db_err)?;
    let native_ids: Vec<_> = ids
        .into_iter()
        .filter(|id| {
            id != session
                || direct_usage
                || record.as_ref().and_then(|r| r.native_session_id.as_deref()) == Some(session)
        })
        .collect();
    let mut result = json!({
        "session":record,"nativeSessionIds":native_ids,"story":story,
        "run":{"state":"unlinked","reason":"the session has no linked run"},
        "models":models.into_values().collect::<Vec<_>>(),
        "cost":{"state":if reason.is_some(){"absent"}else{"present"},"totalUsd":super::cost_rollup::CostRollup::total(&rollup),"source":source(&sources),"reason":reason},
        "tools":{"allowed":{"mcpAllow":record.as_ref().and_then(|r|r.mcp_allow.as_ref()),"bridgeTools":record.as_ref().and_then(|r|r.bridge_tools.as_ref()),
            "unavailable":record.as_ref().map(|r|r.unavailable.iter().filter(|(k,_)| std::string::String::as_str(k)=="mcpAllow" || std::string::String::as_str(k)=="bridgeTools").collect::<BTreeMap<_,_>>())},"used":used,"usedState":used_state,"usedReason":used_reason},
        "tasks":store.tasks_for_session(session)?,
        "retries":null,"unavailable":unavailable
    });
    if let Some(selector) = params.selector()? {
        let page = crate::session_conversation::read_conversation_in(path, dir, session, &selector)?;
        if params.include.contains(&Include::Turns) {
            return turns_answer(&result, page);
        }
        result["conversation"] = serde_json::to_value(page).map_err(|e| e.to_string())?;
    }
    Ok(result)
}

/// One turn as a page lists it: its place, role, time, the start of its text, how many tools it
/// called and how many of those failed, and its model. Never a tool input or result.
fn turn_summary(turn: &codesense_pty_core::agent_cli::output::Turn) -> Value {
    let mut row = json!({
        "index": turn.index,
        "role": turn.role,
        "tsMs": turn.ts_ms,
        "text": Option::map(turn.text.as_deref(), summary_text),
        "toolCalls": Vec::len(&turn.tool_calls),
        "toolErrors": Iterator::count(turn.tool_calls.iter().filter(|call| call.is_error == Some(true))),
        "model": turn.model,
    });
    // A call whose record does not say whether it failed is unknown, never a success.
    let unknown = Iterator::count(turn.tool_calls.iter().filter(|call| Option::is_none(&call.is_error)));
    if unknown > 0 {
        row["toolErrorsUnknown"] = json!(unknown);
    }
    row
}

/// The `unavailable` reasons that explain the light answer's own fields.
fn pick_reasons(unavailable: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in ["story", "timing", "session"] {
        if let Some(reason) = Value::get(unavailable, key) {
            serde_json::Map::insert(&mut out, key.into(), reason.clone());
        }
    }
    Value::Object(out)
}

/// The light answer of `include: [turns]`: the session's identity and timing, its story, and the
/// conversation page with its turns summarised. Its states and reasons are the page's own.
fn turns_answer(
    full: &Value,
    mut page: crate::session_conversation::ConversationPage,
) -> Result<Value, String> {
    let turns: Vec<Value> = std::mem::take(&mut page.turns).iter().map(turn_summary).collect();
    let mut page = serde_json::to_value(page).map_err(|e| e.to_string())?;
    page["turns"] = json!(turns);
    let session = &full["session"];
    Ok(json!({
        "session": {
            "sessionId": session["sessionId"],
            "cli": session["cli"],
            "branch": session["branch"],
            "timing": session["timing"],
        },
        "story": full["story"],
        // The reasons for what is null above: no story link, no timing, no session record.
        "unavailable": pick_reasons(&full["unavailable"]),
        "turns": page,
    }))
}

#[cfg(test)]
mod tests;
