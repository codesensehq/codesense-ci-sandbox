// SPDX-License-Identifier: MIT
//! Reading a stored conversation by session: pages of turns, a jump to a turn or to a tool call by
//! the CLI's own id, and an explicit state for every case where there are no turns to return.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use codesense_pty_core::agent_cli::output::Turn;

use super::{read_stored, Truncation};
use crate::commands::sprint_run::run_transcript::redact_transcript;
use crate::session_usage::conversation::State;
use crate::session_usage::UsageStore;

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 200;

/// What a read can say about a session's conversation. Never an empty list standing in for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Capture could not obtain the session record or its native ID; see the reason.
    Unavailable,
    /// Stored whole.
    Present,
    /// Stored, with part cut by the size limit (see `truncated`).
    Truncated,
    /// Nothing was stored (no such session, no native id or log yet, or the file is gone).
    Absent,
    /// Its retention ran out. The tool-call index remains.
    Expired,
    /// The session's CLI has no reader.
    Unsupported,
}

/// A durable indexed location. This lookup never opens conversation text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexedPointer {
    pub session_id: String,
    pub turn_index: Option<u32>,
    /// When the call ran, from the index.
    pub ts_ms: Option<i64>,
    pub state: Availability,
    pub text_truncated: bool,
}

pub(crate) fn indexed_pointer(
    store: &UsageStore,
    session_id: &str,
    tool_id: &str,
) -> Result<IndexedPointer, String> {
    let record = store.read_session_record(session_id)?;
    let session_id = record
        .map(|r| r.session_id)
        .unwrap_or_else(|| session_id.to_owned());
    let meta = store.read_conversation_meta(&session_id)?;
    let state = match meta.as_ref().map(|m| m.state) {
        Some(State::Present) => Availability::Present,
        Some(State::Truncated) => Availability::Truncated,
        Some(State::Expired) => Availability::Expired,
        Some(State::Unsupported) => Availability::Unsupported,
        Some(State::Unavailable) => Availability::Unavailable,
        None => Availability::Absent,
    };
    let call = store.find_tool_call(&session_id, tool_id)?;
    let turn_index = call.as_ref().map(|r| r.turn_index);
    let text_truncated = turn_index.is_some_and(|i| {
        meta.as_ref()
            .and_then(|m| m.truncated.as_ref())
            .and_then(|t| t.dropped_turns)
            .is_some_and(|(a, b)| (a..=b).contains(&i))
    });
    Ok(IndexedPointer {
        session_id,
        turn_index,
        ts_ms: call.and_then(|r| r.ts_ms),
        state,
        text_truncated,
    })
}

/// Which turns to return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// From the turn at `cursor` (its index; default the first), up to `limit` turns.
    Page {
        cursor: Option<u32>,
        limit: Option<usize>,
    },
    /// From the turn at `index`.
    Turn { index: u32, limit: Option<usize> },
    /// From the turn holding the tool call the CLI knows as `id`.
    ToolUse { id: String, limit: Option<usize> },
}

/// How a jump to a turn or tool call went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lookup {
    /// `turn` or `toolUse`.
    pub kind: String,
    pub requested: String,
    pub found: bool,
    pub turn_index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPage {
    pub session_id: String,
    pub state: Availability,
    /// Why there are no turns, or why a jump found nothing. Set whenever `turns` is empty.
    pub reason: Option<String>,
    pub turns: Vec<Turn>,
    /// The index to pass as `cursor` for the next page; `None` at the end.
    pub next_cursor: Option<u32>,
    /// The turns the store holds (after any cut).
    pub turn_count: u32,
    /// The tool calls the index holds, whatever happened to the text.
    pub tool_call_count: u32,
    pub truncated: Option<Truncation>,
    pub source_format: Option<String>,
    pub unavailable: BTreeMap<String, String>,
    pub lookup: Option<Lookup>,
}

fn empty(session_id: &str, state: Availability, reason: impl Into<String>) -> ConversationPage {
    ConversationPage {
        session_id: session_id.to_owned(),
        state,
        reason: Some(reason.into()),
        turns: Vec::new(),
        next_cursor: None,
        turn_count: 0,
        tool_call_count: 0,
        truncated: None,
        source_format: None,
        unavailable: BTreeMap::new(),
        lookup: None,
    }
}

/// Read `session_id` (a CodeSense session id or the CLI's own) from the store at `store_path` and
/// the conversation files in `dir`.
pub fn read_conversation_in(
    store_path: &Path,
    dir: &Path,
    session_id: &str,
    selector: &Selector,
) -> Result<ConversationPage, String> {
    let store = UsageStore::open(store_path)?;
    let record = store.read_session_record(session_id)?;
    let session = record
        .as_ref()
        .map_or(session_id, |r| r.session_id.as_str());
    let Some(meta) = store.read_conversation_meta(session)? else {
        // A CLI CodeSense never reads is unsupported from the day it launches, not only once a
        // capture has been tried; absent is for a session with no record and for a CLI that is
        // read but has not produced anything yet.
        if let Some(reason) = record
            .as_ref()
            .and_then(|r| r.cli.as_deref())
            .and_then(|cli| cli.parse::<codesense_pty_core::agent_cli::AgentCli>().ok())
            .and_then(super::unsupported_reason)
        {
            return Ok(empty(session, Availability::Unsupported, reason));
        }
        let why = if record.is_some() {
            "no conversation has been stored for this session yet"
        } else {
            "there is no such session"
        };
        return Ok(empty(session, Availability::Absent, why));
    };
    let tool_call_count = store.count_tool_calls(session)?;
    let mut page = match meta.state {
        State::Unavailable => empty(
            session,
            Availability::Unavailable,
            meta.unavailable
                .get("conversation")
                .cloned()
                .unwrap_or_else(|| "capture could not obtain its session record".to_owned()),
        ),
        State::Unsupported => empty(
            session,
            Availability::Unsupported,
            meta.unavailable
                .get("conversation")
                .cloned()
                .unwrap_or_else(|| "this CLI's conversation is not read".to_owned()),
        ),
        State::Expired => empty(
            session,
            Availability::Expired,
            "the conversation passed its retention; its tool-call index remains",
        ),
        State::Present | State::Truncated => match read_stored(dir, session) {
            None => empty(
                session,
                Availability::Absent,
                "the stored conversation file is gone",
            ),
            Some(mut stored) => {
                redact_transcript(&mut stored.transcript);
                select(session, &meta.state, &mut stored, &store, selector)?
            }
        },
    };
    page.tool_call_count = tool_call_count;
    page.source_format = meta.source_format.clone();
    page.unavailable = meta.unavailable.clone();
    page.truncated = meta.truncated.clone();
    if page.state != Availability::Absent && page.turn_count == 0 {
        page.turn_count = meta.turn_count;
    }
    Ok(page)
}

fn select(
    session: &str,
    state: &State,
    stored: &mut super::StoredConversation,
    store: &UsageStore,
    selector: &Selector,
) -> Result<ConversationPage, String> {
    let availability = if *state == State::Truncated {
        Availability::Truncated
    } else {
        Availability::Present
    };
    let all = std::mem::take(&mut stored.transcript.turns);
    let turn_count = u32::try_from(all.len()).unwrap_or(u32::MAX);
    let (start, limit, lookup) = match selector {
        Selector::Page { cursor, limit } => (cursor.unwrap_or(0), *limit, None),
        Selector::Turn { index, limit } => (
            *index,
            *limit,
            Some(Lookup {
                kind: "turn".into(),
                requested: index.to_string(),
                found: all.iter().any(|t| t.index == Some(*index)),
                turn_index: Some(*index),
            }),
        ),
        Selector::ToolUse { id, limit } => {
            let turn_index = indexed_pointer(store, session, id)?.turn_index;
            (
                turn_index.unwrap_or(0),
                *limit,
                Some(Lookup {
                    kind: "toolUse".into(),
                    requested: id.clone(),
                    found: turn_index.is_some_and(|i| all.iter().any(|t| t.index == Some(i))),
                    turn_index,
                }),
            )
        }
    };
    let limit = limit
        .unwrap_or(if lookup.is_some() { 1 } else { DEFAULT_LIMIT })
        .clamp(1, MAX_LIMIT);
    let mut reason = None;
    if let Some(l) = &lookup {
        if !l.found {
            reason = Some(match (&l.turn_index, &stored.truncated) {
                (None, _) => format!("no tool call with id {} in this session", l.requested),
                (Some(i), Some(t)) if t.dropped_turns.is_some_and(|(a, b)| (a..=b).contains(i)) => {
                    format!("turn {i} was dropped by the size limit")
                }
                (Some(i), _) => format!("turn {i} is not in the stored conversation"),
            });
        }
    }
    let rest: Vec<Turn> = if reason.is_some() {
        Vec::new()
    } else {
        all.into_iter()
            .filter(|t| t.index.unwrap_or(0) >= start)
            .collect()
    };
    let more = rest.len() > limit;
    let turns: Vec<Turn> = rest.into_iter().take(limit).collect();
    let next_cursor = more
        .then(|| turns.last().and_then(|t| t.index).map(|i| i + 1))
        .flatten();
    if turns.is_empty() && reason.is_none() {
        reason = Some("no turns at or after that position".to_owned());
    }
    Ok(ConversationPage {
        session_id: session.to_owned(),
        state: availability,
        reason,
        turns,
        next_cursor,
        turn_count,
        tool_call_count: 0,
        truncated: None,
        source_format: None,
        unavailable: BTreeMap::new(),
        lookup,
    })
}
