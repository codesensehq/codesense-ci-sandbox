// SPDX-License-Identifier: MIT
//! P14: the queries a live dashboard may declare and the Mac answers
//! (SW-DP-4b, SW-DP-5a). This is the one list: manifest validation, the
//! link's dispatcher and the hosted section of
//! `get_live_data_contract` all read it.
//!
//! **The dashboard sets the scope, never the viewer.** Each entry calls the
//! same Rust function as its MCP twin, then returns only an allowlist of the
//! twin's fields (`shares`). Free text a viewer does not need (paths, notes,
//! summaries) is left out unless the dashboard's manifest names it in
//! `live.fields.<query>`. Rows are capped. The
//! viewer's args can only narrow what the dashboard allows. Every answer then
//! goes through [`redact`] (DP-INV-10).
//!
//! Adding a query is one `QueryDef` entry here plus its row in the registry's
//! P14 table. An entry that reads a credential store is never added: a test
//! checks this file's source for the names of those modules.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use futures_util::future::BoxFuture;
use regex::Regex;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::shared::secret_scan::{redact_secrets, REDACTION_PLACEHOLDER};

/// What an entry's `run` gets besides its call: the app, for the twins that need it.
#[derive(Clone, Default)]
pub struct LiveCtx {
    pub app: Option<tauri::AppHandle>,
    /// The agent streams' view of this Mac (SW-DP-21). `None`: built from `app`.
    /// The tests set a fixture.
    pub(crate) agents: Option<Arc<dyn super::agents::AgentHost>>,
    pub(crate) inbox: Option<super::inbox::Host>,
    pub(crate) context_index: Option<crate::context_query::ContextIndexService>,
}

impl LiveCtx {
    fn context_service(&self) -> Option<crate::context_query::ContextIndexService> {
        use tauri::Manager;
        self.context_index.clone().or_else(|| {
            self.app
                .as_ref()?
                .try_state::<crate::context_query::ContextIndexService>()
                .map(|s| s.inner().clone())
        })
    }
}

/// Query pins use the same literal/selector matcher for every caller.
pub async fn pins_allow(
    ctx: &LiveCtx,
    pins: &super::manifest::LiveArgs,
    query: &str,
    args: &Value,
    folder: &std::path::Path,
    scope: Option<&super::state::Scope>,
) -> bool {
    use super::manifest::{pin_allows, Pin};
    if pin_allows(pins, query, args, |_, _| false) {
        return true;
    }
    if !pins.get(query).is_some_and(|keys| {
        keys.values()
            .flatten()
            .any(|p| matches!(p, Pin::Selector(_)))
    }) {
        return false;
    }
    let Some(service) = ctx.context_service() else {
        return false;
    };
    let snapshot = service.snapshot();
    let (pins, query, args, folder, scope) = (
        pins.clone(),
        query.to_string(),
        args.clone(),
        folder.to_path_buf(),
        scope.cloned(),
    );
    blocking(None, "live.queryPins", move || {
        Ok(pin_allows(&pins, &query, &args, |selector, id| {
            selector_allows(&snapshot, &folder, scope.as_ref(), selector, id)
        }))
    })
    .await
    .unwrap_or(false)
}

/// Resolve inside one immutable snapshot, using the context engine's epic relationship rule.
fn selector_allows(
    snapshot: &crate::context_query::index::Snapshot,
    folder: &std::path::Path,
    scope: Option<&super::state::Scope>,
    selector: &super::manifest::Selector,
    id: &str,
) -> bool {
    use super::manifest::Selector;
    if !snapshot.ready {
        return false;
    }
    let Ok((org, team)) = check_scope(scope, team_of(&snapshot.roots, folder)) else {
        return false;
    };
    let mut query = json!({"where":{"orgs":[org],"teams":[team],"layers":["team"],"kinds":["story","bug","spike"]}, "filters":[{"key":"id","op":"eq","value":id}], "select":["team"], "limit": 2});
    match selector {
        Selector::Epic { epic } => {
            query["relationships"] = json!({"parentEpic":epic});
        }
        Selector::Team { team: selected } if selected == &team => {}
        Selector::Team { .. } => return false,
    }
    let Ok(query) = serde_json::from_value(query) else {
        return false;
    };
    let viewer = crate::context_query::Viewer {
        page: Some(folder.display().to_string()),
        ..Default::default()
    };
    let Ok(result) = snapshot.query(
        &query,
        &viewer,
        &crate::context_query::engine::DiskBodyReader,
    ) else {
        return false;
    };
    result.total == 1
        && result.rows.len() == 1
        && result.rows[0].id == id
        && match selector {
            Selector::Epic { .. } => true,
            Selector::Team { .. } => {
                result.rows[0].fields.get("team").and_then(Value::as_str) == Some(team.as_str())
            }
        }
}

/// One relayed query, as the dispatcher hands it to an entry.
#[derive(Clone, Default)]
pub struct Call {
    /// The viewer's args (P11 `args`).
    pub args: Value,
    /// The fields the dashboard shares for this query: for `queryContext` the
    /// frontmatter keys (or the default set), for the others the optional
    /// fields the manifest names.
    pub fields: Vec<String>,
    /// The dashboard's folder, which fixes `queryContext`'s scope.
    pub folder: PathBuf,
    /// The org and team saved for `queryContext` at publish (`None`: none).
    pub scope: Option<super::state::Scope>,
    /// The K20 slot. Blocking work keeps a clone until it ends, so the slot is
    /// not freed while a dropped query still runs on a blocking thread.
    pub hold: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    /// The link's hosted git reads (SV-ENG-121): one permit per read, kept until it ends.
    pub walks: Option<Arc<tokio::sync::Semaphore>>,
    /// How long such a read runs on after its answer timed out (`None`: 120 s).
    pub walk_deadline: Option<std::time::Duration>,
}

/// Why a query was not answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// The args do not parse or ask for more than the dashboard allows (P11 `invalid_args`).
    InvalidArgs(String),
    /// The cursor's snapshot is no longer kept: the page restarts without it (`stale_cursor`).
    StaleCursor,
    /// The twin failed (P11 `failed`).
    Failed(String),
    /// Not covered by the saved live block (P11 `not_approved`): `queryContext`
    /// with no team saved, or whose folder now resolves to another team.
    NotApproved(String),
    /// No room for the query now (a blocking or git read still running): retry (P11 `busy`).
    Busy,
    /// A stream refused with this P16 `so` error (SW-DP-21): `not_found` (no
    /// such handle, or not this dashboard's team) or `no_team`.
    Refused(&'static str),
}

/// An entry's runner.
pub type RunFn = fn(LiveCtx, Call) -> BoxFuture<'static, Result<Value, QueryError>>;

/// Which extra fields a manifest may name for a query (`live.fields.<query>`).
#[derive(Debug, Clone, Copy)]
pub enum Fields {
    /// Frontmatter keys (`queryContext`). Without a list, `DEFAULT_CONTEXT_FIELDS`.
    Frontmatter,
    /// Only these optional fields.
    Named(&'static [&'static str]),
}

/// One query the Mac can answer to a live dashboard.
pub struct QueryDef {
    /// The P13 name, which is also the in-app `window.codesense` verb.
    pub name: &'static str,
    /// One line for the live-data contract.
    pub describe: &'static str,
    /// Exactly what an answer holds, for the contract.
    pub shares: &'static str,
    pub fields: Fields,
    pub args: &'static [&'static str],
    pub run: RunFn,
}

impl std::fmt::Debug for QueryDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryDef")
            .field("name", &self.name)
            .finish()
    }
}

/// The story-id arguments that accept scoped selectors.
pub const SELECTOR_ARGS: &[(&str, &str)] = &[("storyChanges", "storyId"), ("storyTotals", "storyIds")];

/// Queries that require a scope pin before answering viewers.
pub const PIN_REQUIRED_ARGS: &[(&str, &str)] = &[
    ("storyChanges", "storyId"),
    ("storyTotals", "storyIds"),
    ("whyLine", "repo"),
];

/// Arguments that take a list: every item must pass the argument's pin.
pub const LIST_ARGS: &[(&str, &str)] = &[("storyTotals", "storyIds"), ("storyChanges", "only")];

/// The most rows any answer holds.
pub const MAX_ROWS: usize = 500;

#[cfg(test)]
mod inbox_contract_tests {
    use super::*;

    #[test]
    fn inbox_is_a_declared_query_and_value_stream_with_opt_in_text() {
        let def = find(CATALOG, "inbox").expect("inbox query");
        assert!(effective_fields(def, None).is_empty());
        assert!(field_refusal(def, "text").is_none());
        assert!(field_refusal(def, "onClick").is_some());
        assert_eq!(
            find_stream(STREAMS, "inbox").expect("inbox stream").kind,
            StreamKind::Value
        );
    }
}
/// `sessionUsage` and `runs` without a `limit`.
pub const DEFAULT_ROWS: usize = 50;
/// `queryContext`'s frontmatter fields when the manifest lists none.
pub const DEFAULT_CONTEXT_FIELDS: &[&str] = &["status", "title", "type", "owner", "updated"];
/// Frontmatter keys a manifest may never name: they would pull body text or
/// relationship paths into an answer.
pub const FORBIDDEN_CONTEXT_FIELDS: &[&str] = &[
    "summary",
    "relationships",
    "body",
    "text",
    "path",
    "content",
];

/// The catalog the app uses.
pub const CATALOG: &[QueryDef] = &[
    QueryDef {
        name: "inbox",
        describe: "The dashboard team's human inbox, session inboxes and role queues",
        shares: "{messages: [{messageId, from, fromName, summary, kind, createdAt, unread, inReplyTo, delivery?: {state}}], humanUnread}. No bodies unless fields: text is declared. fields: meta adds to, toName, replyTo?, replyToName? and contentType (plain, markdown, html or null). fields: media adds attachments [{kind: image, alt, state (cached, pending, blocked or failed), mime?, sha256?, width?, height?, bytes?}], never a source URL, host or failure reason. fields: reactions adds reactions [{emoji, count, mine}] (mine: the human reacted), never who reacted. fields: faces adds sender: {face} to the rows whose sender wears one: {sha256, mime: image/png, width, height} for a picture (its bytes come from the media query), {emoji} or {colour} (#rrggbb); only for senders of the rows returned, no roster. Team fixed by the dashboard; args scope (to-human, to-agents, all, or human: only messages to or from the human, filtered on the Mac so no agent-to-agent mail leaves it), agentId, sessionId, role and limit only narrow; any other scope is query_invalid. At most 500 rows, newest first; a list cut by that limit or by size carries truncated, returned and total. Reads mark nothing read. Redacted",
        fields: Fields::Named(&["text", "meta", "media", "reactions", "faces"]),
        args: &["scope", "agentId", "sessionId", "role", "limit"],
        run: super::inbox::query,
    },
    QueryDef {
        name: "media",
        describe: "One image cached on this Mac for a message in the dashboard team's inbox, in chunks",
        shares: "args {messageId, sha256, offset?}: the image's bytes as base64 (at most 180 KB raw per answer, from offset) with mime, width, height, total, length and done. Only an image attached to a message in this dashboard team's inbox whose attachment is cached on the Mac (never fetched for the viewer), at most 5 MB and a GIF, WebP, PNG or JPEG (never SVG). Also serves the picture face of the sender of a message in that inbox (a PNG drawn on the Mac, at most 96 px; the sha256 is the one the row's sender.face names), needing live.fields.inbox: [faces]. Attachments need live.fields.inbox: [media]. Anything else answers {refused: not_found | not_cached | too_large}. Reads mark nothing read",
        fields: Fields::Named(&[]),
        args: &["messageId", "sha256", "offset"],
        run: super::inbox::media::query,
    },
    QueryDef {
        name: "storyChanges",
        describe: "A story's linked sessions, tasks and complete cost/time totals",
        shares: "args {storyId,limit?,cursor?,only?}; only (tasks, sessions, commits, prs, branches, worktrees, actual, reviews) answers just those arrays with totals, leaves the others out, and reads no git unless a git array is asked (without one totals.commits, files and lines are null with unavailable.commits, files and lines saying why, and a cursor is stale); reviews is the story's complete review rounds as {round,phase,scope,skill,subjectRev,high,medium,low,dismissed,converged,reportedAt} (no id, subject, reviewer, session or finding ids); paged commits projected to sha,authoredAt,repo-relative tracked files,sessionIds,basis (no subjects); branches to PR numbers, ref availability, merged SHA and basis only; worktrees to state and basis only; prs lists all observed merge/squash PR facts with source and basis (no branch names); planned/actual repo-relative file facts and decision numbers; all linked sessions projected to sessionId,basis,models,cost,timing; tasks projected to taskId,parentId,reviews. Complete totals, including totals.lines (counts of AI lines by runtime, human and unattributed lines over every commit, or null with an unavailable.lines reason), prs[].mergedAt (epoch ms), unavailable reasons and historyAsOf (fetchedAt, originHead). No account, record, absolute host paths, worktree paths, branch names, commit subjects, task goals, progress notes or idle gaps. No default row cap; an oversized answer fails rather than labelling partial rows complete",
        fields: Fields::Named(&[]),
        args: &["storyId", "limit", "cursor", "only"],
        run: story_changes,
    },
    QueryDef {
        name: "storyTotals",
        describe: "Many stories' complete cost/time/commit totals in one read, from the Mac's story totals index",
        shares: "args {storyIds} (each passing the storyTotals.storyIds pin; a hosted ask holds the ids that fit the hosted 2 KB args limit (about 120 to 150), so pages call CS.storyTotals(ids), which asks in parts and merges one answer): {stories: {<id>: {totals (the storyChanges totals: counts, cost, time and lines counts), unavailable ({field: index into reasons}, only the reasons storyChanges publishes), computedAtMs, refreshing?, mergedAt (latest merge, epoch ms) or mergedAtReason, prs (newest 5 merged PRs as {number, merged}), prsTotal, runtimes (CLI ids), firstActivityAt and lastActivityAt (epoch ms over sessions' usage, commits, merges, tasks and review rounds; null with activityReason when none is recorded, or \"refreshing\" while an older row is recomputed), openSessions (sessions with observed timing and no end that were active in the 15 minutes before computedAtMs, by usage or turn; null while refreshing), linesInProgress (lines on story branches not merged yet, shaped like totals.lines; after a squash or rebase merge, lines stay here while the story's branch ref still exists, local or on origin, until it is deleted; null with linesInProgressReason)} | {pending: true} (not indexed yet, never zero) | {error: failed}}, reasons, pending (the pending ids), indexing {done, total}, truncated? and rest? (the ids left past 240 KB: ask again)}. Read from the index, never from git. No sessions, tasks, commits, files, paths, branch names or subjects",
        fields: Fields::Named(&[]),
        args: &["storyIds"],
        run: story_totals,
    },
    QueryDef {
        name: "whyLine",
        describe: "Committed line attribution and linked session facts from a local checkout",
        shares: "args {repo,rev?,story?,path,start,end}; repo,rev,revSource (explicit, mergedSha, branch or defaultBranch; an omitted rev never reads the checkout's HEAD),resolvedCommit,repo-relative path,notesRef (opaque snapshot id of the sense and ai notes refs),historyAsOf (fetchedAt, originHead),ranges projected to start,end,basis,commit,tool,model,sessionId,link,reason,story,candidateSessions,turn (sessionId,toolUseId,turn,basis,taskId,taskBasis,reason only),unavailable (turn only); sessions keyed by id with sessionId,basis,models,cost and allow-listed timing. Unavailable reads share status, reason and historyAsOf. No native session ids, records, accounts, host paths, commit subjects or conversation. Local objects only, never fetched",
        fields: Fields::Named(&[]),
        args: &["repo", "rev", "story", "path", "start", "end"],
        run: why_line,
    },
    QueryDef {
        name: "setup",
        describe: "Whether line attribution and turn-link capture are working on this Mac",
        shares: "{attribution: {status: {kind, reason? (present only when the status is off; per-launch states appear on worktrees rows, not here)}, gitAiVersion, daemonReachable, supportedVersions, notesRef, capture: {lastSweepAt, trigger (startup|watcher), clonesSwept, logsSeen, logsNew, traceRowsStored, sessionMappingRowsStored, watchErrors, lastError}}}. Capture counts are since the app started (empty with lastSweepAt null until the first sweep); lastError is a fixed phase message. notesRef is the local notes ref (sense). Not the git-ai path, the previous-binary receipt, warnings, any status message text or the repos set to push notes.",
        fields: Fields::Named(&[]),
        args: &[],
        run: setup,
    },
    QueryDef {
        name: "sessionUsage",
        describe: "Token use and spend per agent session on this Mac",
        shares: "per session: id, CLI, org, team, agent, models, first and last activity, message and token counts, cost; per day and in total: tokens and cost. Not the story each session was spent on (fields: story, its id, title, status and epic) or the working folder (fields: cwd). Newest activity first, at most 500 sessions, 50 by default. A cut list carries truncated, returned and total; days and totals still cover every session",
        fields: Fields::Named(&["cwd", "story"]),
        args: &["sinceMs", "untilMs", "sessionId", "cwd", "story", "limit", "cursor"],
        run: session_usage,
    },
    QueryDef {
        name: "sessions",
        describe: "The agent sessions CodeSense is running",
        shares: "per session: handle, name, state, who launched it, story, role, agent, machine name, unread count, progress phase and step, deadline. Not the workspace (fields: workspace) or the progress note (fields: progressNote). In handle order, at most 500. A cut list carries truncated, returned and total",
        fields: Fields::Named(&["workspace", "progressNote"]),
        args: &["launchedBy"],
        run: sessions,
    },
    QueryDef {
        name: "runs",
        describe: "Recent agent runs and how each ended",
        shares: "{runs: [...]}, one row per run: story, run id, status, exit code, start and end times, how it ended, automation and trigger. Not the summary (fields: summary) or the workspace (fields: workspace). Newest first, at most 500, 50 by default. A cut list carries truncated, returned and total",
        fields: Fields::Named(&["summary", "workspace"]),
        args: &["limit"],
        run: runs,
    },
    QueryDef {
        name: "queryContext",
        describe: "Files in this dashboard's own team context (stories, epics, docs) by their metadata",
        shares: "per file in {scope} only (never the private space): id, path inside the team, file name, kind, modified time, and only the frontmatter fields the dashboard lists (default: status, title, type, owner, updated). No file text, no summaries, no search, at most 500 rows. total and truncated are always present; a cut list also carries returned",
        fields: Fields::Frontmatter,
        args: &["match", "sort", "limit", "groupBy", "where", "filters", "select", "cursor"],
        run: query_context,
    },
];

/// P18: one catalog shared by manifest validation, dispatch and authoring docs.
pub struct ActionDef {
    pub name: &'static str,
    pub describe: &'static str,
}
pub const ACTIONS: &[ActionDef] = &[
    ActionDef { name: "ownerMessage", describe: "Built-in owner-only plain-text send or reply within the dashboard team; no action declaration" },
    ActionDef { name: "ownerReact", describe: "Built-in owner-only emoji reaction (tapback) on a message of the dashboard team's inbox, one of 😂 ❤️ 👍 😮 🎉 🔥; toggles; no action declaration" },
    ActionDef { name: "ownerMarkRead", describe: "Built-in owner-only: marks messages to the human read in the dashboard team's inbox, 1 to 200 ids per call, idempotent; every id must be a message to the human in this team, else not_found; no action declaration" },
    ActionDef { name: "sendMessage", describe: "Owner-only plain-text send to a declared recipient set" },
    ActionDef {
        name: "setFields",
        describe: "Set declared metadata fields on a validated team file",
    },
    ActionDef {
        name: "launchAgent",
        describe: "Launch the declared agent on a validated team id",
    },
    ActionDef {
        name: "runRoutine",
        describe: "Fire the declared routine with fixed variables",
    },
];

/// Where a `shares` text names the org and team `queryContext` reads.
pub const SCOPE_MARK: &str = "{scope}";

/// The org and team `queryContext` may read: the one the folder resolves to
/// now, only when it is the one saved at publish. No saved scope (it was
/// unknown at publish), or a folder that now resolves elsewhere (a deeper team
/// repo attached later, or none), is `not_approved`.
pub fn check_scope(
    approved: Option<&super::state::Scope>,
    now: Option<(String, String)>,
) -> Result<(String, String), QueryError> {
    let approved = approved.ok_or_else(|| {
        QueryError::NotApproved("no team was known when the dashboard was published".into())
    })?;
    match now {
        Some((org, team)) if org == approved.org && team == approved.team => Ok((org, team)),
        _ => Err(QueryError::NotApproved(
            "the dashboard's folder no longer resolves to the team saved at publish".into(),
        )),
    }
}

/// The contract's wording of [`SCOPE_MARK`]: the rule, not one dashboard's team.
pub fn shares_rule(q: &QueryDef) -> String {
    q.shares.replace(
        SCOPE_MARK,
        "the one org and team whose context repo folder holds the dashboard folder (the org and team query_context lists for a file there), worked out when publish_dashboard runs",
    )
}

/// The entry named `name` in `catalog`, if any.
pub fn find<'a>(catalog: &'a [QueryDef], name: &str) -> Option<&'a QueryDef> {
    catalog.iter().find(|q| q.name == name)
}

/// Why `field` cannot be named for `def` in a manifest, if it cannot.
pub fn field_refusal(def: &QueryDef, field: &str) -> Option<&'static str> {
    static NAME: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9_.-]{0,63}$").expect("compiles"));
    match def.fields {
        Fields::Frontmatter if !NAME.is_match(field) => Some("is not a frontmatter key"),
        Fields::Frontmatter if FORBIDDEN_CONTEXT_FIELDS.contains(&field) => {
            Some("would expose file text or paths")
        }
        Fields::Frontmatter => None,
        Fields::Named(allowed) if allowed.contains(&field) => None,
        Fields::Named(_) => Some("is not an optional field of this query"),
    }
}

/// The fields an answer to `def` may carry, from the approved list.
pub fn effective_fields(def: &QueryDef, approved: Option<&Vec<String>>) -> Vec<String> {
    match (def.fields, approved) {
        (Fields::Frontmatter, None) => DEFAULT_CONTEXT_FIELDS
            .iter()
            .map(|f| f.to_string())
            .collect(),
        (_, Some(list)) => list.clone(),
        (Fields::Named(_), None) => Vec::new(),
    }
}

// ---- Streams (SW-DP-20, P17) ----

/// How a stream's frames read (P16): a whole JSON value on each change, or a
/// snapshot followed by ordered output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Value,
    Bytes,
}

impl StreamKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamKind::Value => "value",
            StreamKind::Bytes => "bytes",
        }
    }
}

/// What an open stream yields to the link's dispatcher, which paces it (K28)
/// and frames it (P16).
#[derive(Debug, Clone, PartialEq)]
pub enum StreamItem {
    /// `value` streams: the whole current value.
    Value(Value),
    /// `bytes` streams: everything a watcher needs to show now. Yielded first,
    /// and again each time the call's `snap` is notified. `cols` and `rows`
    /// are for terminals (SW-DP-21).
    Snapshot {
        vt: String,
        cols: Option<u16>,
        rows: Option<u16>,
    },
    /// `bytes` streams: output after the last snapshot.
    Bytes(String),
    /// `bytes` streams: the terminal was resized after the last snapshot (P16 `rs`, SW-DP-21).
    Resized { cols: u16, rows: u16 },
    /// The stream ended on its own (P16 `end` reason: `exited`, `closed`, `not_found`).
    End(&'static str),
}

/// One stream the relay opened (P16 `s`), as the dispatcher hands it to an entry.
#[derive(Clone, Default)]
pub struct StreamCall {
    /// The viewer's args (P16 `args`).
    pub args: Value,
    /// Optional fields from the approved live block, never from viewer args.
    pub fields: Vec<String>,
    /// The dashboard's folder.
    pub folder: PathBuf,
    /// The org and team saved with the block (see `Call::scope`).
    pub scope: Option<super::state::Scope>,
    /// Notified when a fresh snapshot is needed (a relay `snap`, or the
    /// dispatcher's own gap). A `bytes` stream yields `Snapshot` next; a
    /// `value` stream yields its current value again.
    pub snap: Arc<tokio::sync::Notify>,
}

/// An open stream's items.
pub type Items = futures_util::stream::BoxStream<'static, StreamItem>;

/// An entry's opener. Args that do not parse are `InvalidArgs`. It may read
/// the app before it answers (SW-DP-21: the handle's team), so it is async; the
/// link never waits on it.
pub type OpenFn = fn(LiveCtx, StreamCall) -> BoxFuture<'static, Result<Items, QueryError>>;

/// One stream a live dashboard may declare in `live.streams` and the Mac opens
/// (P17). The same pattern as `QueryDef`: manifest validation, the contract
/// and the dispatcher all read [`STREAMS`].
pub struct StreamDef {
    /// The P15 `stream` name.
    pub name: &'static str,
    /// One line for the live-data contract.
    pub describe: &'static str,
    /// Exactly what its frames hold, for the contract.
    pub shares: &'static str,
    pub kind: StreamKind,
    pub open: OpenFn,
}

impl std::fmt::Debug for StreamDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamDef")
            .field("name", &self.name)
            .finish()
    }
}

/// SW-DP-21 (P17): the agent streams.
const WORKSPACES: StreamDef = StreamDef {
    name: "workspaces",
    describe: "The workspaces open on this Mac in the dashboard's own team, pushed when they change",
    shares: "{workspaces: [{id, name, story?, branch?, sessions: [handle], tabs}]}: per open workspace of the team whose context repo holds the dashboard folder: its id (an opaque key when the workspace id holds a folder path, so no local path is shared), its name (the story's title for a story workspace, else the folder's name for a folder workspace, else the id), the story id and its worktree branch when it has them, the handles of its watchable sessions and its tab count. Never other teams' workspaces or another machine's. Redacted",
    kind: StreamKind::Value,
    open: super::agents::open_workspaces,
};
const TERMINALS: StreamDef = StreamDef {
    name: "terminals",
    describe: "The agent sessions running on this Mac in the dashboard's own team, pushed when they change",
    shares: "{terminals: [row]}: per session with a live terminal on this Mac whose launch team is the dashboard's team: the sessions query's fields (handle, name, state, who launched it, story, role, avatar, agent, machine, unread, progress phase and step, deadline), workspace {id, name, story?, branch?} and watchable: true. Never sessions of another team, with no team or on another machine. Redacted",
    kind: StreamKind::Value,
    open: super::agents::open_terminals,
};
const ACTION_STREAM: StreamDef = StreamDef {
    name: "actions", describe: "Recent dashboard actions and their progress", shares: "Last 50 calls: time, action, id, role, outcome, handle, run id and state; never email or prompts", kind: StreamKind::Value, open: super::actions::open_stream,
};
const TERMINAL: StreamDef = StreamDef {
    name: "terminal",
    describe: "One session's terminal, read-only: its screen and recent scrollback, then live output",
    shares: "args {handle} (a handle from terminals): a snapshot of the most recent 2,000 lines (at most 1 MB, in parts with cols and rows), then the terminal's output, and resizes. The snapshot and the output are redacted (redaction recognises secrets by shape and labels, so it can't promise to hide every one). A handle that is unknown or not in the dashboard's team ends not_found; the session's exit ends exited. Watching never types or resizes",
    kind: StreamKind::Bytes,
    open: super::agents::open_terminal,
};
const INBOX: StreamDef = StreamDef {
    name: "inbox",
    describe: "The dashboard team's inbox, pushed on change without marking mail read",
    shares: "The inbox query's projection, including text only with live.fields.inbox: [text], to/toName/replyTo/replyToName/contentType only with [meta], image attachments (hash and size only) only with [media], reactions [{emoji, count, mine}] only with [reactions] and sender.face only with [faces]. Same strict args (including scope human) and team scope; redacted. Re-read while watched using the shared list poller",
    kind: StreamKind::Value,
    open: super::agents::open_inbox,
};

/// The streams the app opens (P17). The tests add the `tick` fixtures, which
/// prove the channel (SW-DP-20 AC-4).
#[cfg(not(test))]
pub const STREAMS: &[StreamDef] = &[WORKSPACES, TERMINALS, TERMINAL, INBOX, ACTION_STREAM];
#[cfg(test)]
pub const STREAMS: &[StreamDef] = &[
    ACTION_STREAM,
    WORKSPACES,
    TERMINALS,
    TERMINAL,
    INBOX,
    StreamDef {
        name: "tick",
        describe: "Test stream: a snapshot, then output on a timer",
        shares: "fixture text only",
        kind: StreamKind::Bytes,
        open: tick::open,
    },
    StreamDef {
        name: "tickValue",
        describe: "Test stream: a counter value on a timer",
        shares: "a counter",
        kind: StreamKind::Value,
        open: tick::open_value,
    },
];

/// The stream named `name` in `streams`, if any.
pub fn find_stream<'a>(streams: &'a [StreamDef], name: &str) -> Option<&'a StreamDef> {
    streams.iter().find(|s| s.name == name)
}

/// The `tick` fixtures (tests only).
#[cfg(test)]
pub(crate) mod tick {
    use super::*;
    use futures_util::StreamExt;
    use std::time::Duration;

    pub const TICK_ARGS: &[&str] = &[
        "chunk",
        "every_ms",
        "items",
        "pad",
        "panic_after",
        "open_delay_ms",
        "open_panic",
    ];

    /// `{chunk?, every_ms?, items?, pad?, panic_after?}`: `items` outputs
    /// (unbounded without it) of `chunk` bytes (default 10) every `every_ms`
    /// (default 100, 0: as fast as the dispatcher reads). `tickValue` adds `pad`
    /// bytes of text to each value. `panic_after` panics the source once it has
    /// sent that many, for the failed-task tests.
    #[derive(serde::Deserialize, Default)]
    struct TickArgs {
        chunk: Option<usize>,
        every_ms: Option<u64>,
        items: Option<u64>,
        pad: Option<usize>,
        /// Tests: the stream task panics after this many outputs.
        panic_after: Option<u64>,
        /// Tests: the open takes this long (SW-DP-21: opens run off the link loop).
        open_delay_ms: Option<u64>,
        /// Tests: the opener panics.
        open_panic: Option<bool>,
    }

    struct Tick {
        first: bool,
        sent: u64,
        chunk: usize,
        every: Duration,
        items: Option<u64>,
        snap: Arc<tokio::sync::Notify>,
        value: bool,
        pad: usize,
        panic_after: Option<u64>,
    }

    fn start(call: StreamCall, value: bool) -> Result<Tick, QueryError> {
        let args: TickArgs = strict(call.args, TICK_ARGS)?;
        Ok(Tick {
            first: true,
            sent: 0,
            chunk: args.chunk.unwrap_or(10),
            every: Duration::from_millis(args.every_ms.unwrap_or(100)),
            items: args.items,
            snap: call.snap,
            value,
            pad: args.pad.unwrap_or(0),
            panic_after: args.panic_after,
        })
    }

    fn whole(t: &Tick) -> StreamItem {
        if t.value {
            value_of(t)
        } else {
            StreamItem::Snapshot {
                vt: format!("snapshot after {}", t.sent),
                cols: None,
                rows: None,
            }
        }
    }

    fn value_of(t: &Tick) -> StreamItem {
        if t.pad == 0 {
            StreamItem::Value(json!({"n": t.sent}))
        } else {
            StreamItem::Value(json!({"n": t.sent, "pad": "x".repeat(t.pad)}))
        }
    }

    fn run(t: Tick) -> futures_util::stream::BoxStream<'static, StreamItem> {
        futures_util::stream::unfold(t, |mut t| async move {
            if t.first {
                t.first = false;
                let item = whole(&t);
                return Some((item, t));
            }
            let more = t.items.is_none_or(|n| t.sent < n);
            tokio::select! {
                biased;
                _ = t.snap.notified() => {
                    let item = whole(&t);
                    Some((item, t))
                }
                _ = tokio::time::sleep(t.every), if more => {
                    t.sent += 1;
                    if t.panic_after.is_some_and(|n| t.sent > n) {
                        panic!("tick: panic_after");
                    }
                    let item = if t.value {
                        value_of(&t)
                    } else {
                        StreamItem::Bytes("x".repeat(t.chunk))
                    };
                    Some((item, t))
                }
            }
        })
        .boxed()
    }

    pub fn open(_ctx: LiveCtx, call: StreamCall) -> BoxFuture<'static, Result<Items, QueryError>> {
        Box::pin(async move {
            let args: TickArgs = strict(call.args.clone(), TICK_ARGS)?;
            if args.open_panic == Some(true) {
                panic!("tick: open_panic");
            }
            if let Some(ms) = args.open_delay_ms {
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            Ok(run(start(call, false)?))
        })
    }

    pub fn open_value(
        _ctx: LiveCtx,
        call: StreamCall,
    ) -> BoxFuture<'static, Result<Items, QueryError>> {
        Box::pin(async move { Ok(run(start(call, true)?)) })
    }
}

/// The query's argument keys, shared by parsing and manifest validation.
pub fn query_args(name: &str) -> &'static [&'static str] {
    find(CATALOG, name).expect("catalog query").args
}

/// Parse `args`. `null` is no args. Anything but an object, a key outside
/// `keys`, or a value of the wrong type is `invalid_args`.
pub fn strict<T: DeserializeOwned>(args: Value, keys: &[&str]) -> Result<T, QueryError> {
    let args = match args {
        Value::Null => Value::Object(Default::default()),
        Value::Object(map) => {
            if let Some(key) = map.keys().find(|k| !keys.contains(&k.as_str())) {
                return Err(QueryError::InvalidArgs(format!("unknown argument {key}")));
            }
            Value::Object(map)
        }
        _ => return Err(QueryError::InvalidArgs("args must be an object".into())),
    };
    serde_json::from_value(args).map_err(|e| QueryError::InvalidArgs(e.to_string()))
}

/// Run `f` on a blocking thread holding the call's K20 slot until it returns.
pub(crate) async fn blocking<T, F>(
    call_hold: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    label: &'static str,
    f: F,
) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    crate::commands::await_blocking(label, move || {
        let _slot = call_hold;
        f()
    })
    .await
}

/// `value`'s object with only `keep` (and the `optional` keys the manifest named).
fn pick(value: &Value, keep: &[&str], optional: &[String]) -> Value {
    let Some(map) = value.as_object() else {
        return Value::Null;
    };
    let mut out = Map::new();
    for (k, v) in map {
        if keep.contains(&k.as_str()) || optional.iter().any(|o| o == k) {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

fn capped(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_ROWS).min(MAX_ROWS)
}

/// Room kept under [`super::live::MAX_ANSWER_BYTES`] for the `truncated` and `returned` keys
/// a cut adds and for the commas between rows.
const ANSWER_HEADROOM: usize = 1024;

/// SV-INP-38: the prefix of `rows` (in the order given) that fits the hosted answer cap
/// beside `envelope`, the answer with its rows left empty. Every row is redacted first and
/// sized as the Mac will send it, since redaction can lengthen a value. The first row is always
/// kept, so one row too big for the cap still reaches the dispatcher's `too_large`.
pub(crate) fn fit_rows(rows: Vec<Value>, envelope: &Value) -> Vec<Value> {
    let fixed = serde_json::to_vec(&redact(envelope.clone()))
        .map(|b| b.len())
        .unwrap_or(usize::MAX);
    let mut bytes = fixed.saturating_add(ANSWER_HEADROOM);
    let mut kept = Vec::with_capacity(rows.len());
    for row in rows {
        let row = redact(row);
        // +1 for the separating comma.
        let size = serde_json::to_vec(&row)
            .map(|b| b.len())
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        if bytes.saturating_add(size) > super::live::MAX_ANSWER_BYTES && !kept.is_empty() {
            break;
        }
        bytes = bytes.saturating_add(size);
        kept.push(row);
    }
    kept
}

/// SV-ENG-61: the rows a hosted list answers with, and how many matched before any cut.
pub(crate) struct Bounded {
    rows: Vec<Value>,
    matched: usize,
}

/// The first `cap` of `rows`, fitted to the hosted answer cap beside `envelope` ([`fit_rows`]).
/// `matched` is how many rows the source had before any cut, and at least `rows`' count.
pub(crate) fn bounded_rows(
    rows: impl IntoIterator<Item = Value>,
    matched: usize,
    cap: usize,
    envelope: &Value,
) -> Bounded {
    let rows: Vec<Value> = rows.into_iter().take(cap).collect();
    let matched = matched.max(rows.len());
    Bounded {
        rows: fit_rows(rows, envelope),
        matched,
    }
}

impl Bounded {
    /// How many rows survived the cut, for deriving the cursor after the last one sent.
    pub(crate) fn kept(&self) -> usize {
        self.rows.len()
    }

    /// Puts one page of a paged list into `answer[key]`. Every page says `total` (all matches),
    /// `returned` (the rows on this page) and `truncated` (this page is not everything), and
    /// `nextCursor` only when more rows follow.
    pub(crate) fn put_page_in(self, answer: &mut Value, key: &str, next_cursor: Option<String>) {
        answer["truncated"] = json!(self.rows.len() < self.matched);
        answer["returned"] = json!(self.rows.len());
        answer["total"] = json!(self.matched);
        if let Some(cursor) = next_cursor {
            answer["nextCursor"] = json!(cursor);
        }
        answer[key] = Value::Array(self.rows);
    }

    /// Puts the rows into `answer[key]`. When any were cut, by the limit or by bytes, the answer
    /// also says so: `truncated: true`, `returned` (the rows sent) and `total` (the rows before the cut).
    pub(crate) fn put_in(self, answer: &mut Value, key: &str) {
        if self.rows.len() < self.matched {
            answer["truncated"] = json!(true);
            answer["returned"] = json!(self.rows.len());
            answer["total"] = json!(self.matched);
        }
        answer[key] = Value::Array(self.rows);
    }
}

const USAGE_SESSION_KEYS: &[&str] = &[
    "sessionId",
    "cli",
    "org",
    "team",
    "agent",
    "models",
    "firstAtMs",
    "lastAtMs",
    "messages",
    "tokens",
    "costUsd",
    "unpricedModels",
];

/// Publish only the declared facts; private records, paths and free text stay on the Mac.
/// Published session facts shared by evidence queries; excludes records and account identifiers.
fn project_session(s: &Value) -> Value {

            let timing = s["session"]["timing"].as_object().map(|timing| {
                ["startedAtMs", "endedAtMs", "wallMs", "activeMs", "idleMs", "turnCount", "toolCallCount", "timingSource"]
                    .into_iter().filter_map(|key| timing.get(key).map(|v| (key.to_owned(), v.clone())))
                    .collect::<Map<String, Value>>()
            }).map(Value::Object).unwrap_or(Value::Null);
            json!({
                "sessionId":s["session"]["sessionId"].as_str().or_else(||s["nativeSessionIds"][0].as_str()),
                "basis":s["basis"],"models":s["models"],"cost":s["cost"],"timing":timing
            })

}

fn setup(ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QueryError>> {
    Box::pin(setup_for(ctx.app, call))
}

#[cfg(test)]
pub(crate) async fn setup_answer_for_test<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    args: Value,
) -> Result<Value, QueryError> {
    setup_for(Some(app), Call { args, ..Default::default() }).await
}

async fn setup_for<R: tauri::Runtime>(
    app: Option<tauri::AppHandle<R>>,
    call: Call,
) -> Result<Value, QueryError> {
    let _: Value = strict(call.args, query_args("setup"))?;
    let app = app.ok_or_else(|| QueryError::Failed("the app is not running".into()))?;
    let _slot = call.hold;
    let doctor = crate::mcp::setup_tools::read_local_doctor(&app)
        .await
        .map_err(QueryError::Failed)?;
    Ok(crate::attribution::setup::shared(&doctor))
}

fn why_line(ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QueryError>> {
    Box::pin(why_line_for(ctx.app, call))
}

async fn why_line_for<R: tauri::Runtime>(
    app: Option<tauri::AppHandle<R>>,
    call: Call,
) -> Result<Value, QueryError> {
    why_line_for_with(app, call, |app, params, folder, hold| async move {
        crate::commands::data_read_model::why_line_holding(app, params, None, Some(folder), hold)
            .await
    })
    .await
}

async fn why_line_for_with<R, F, Fut>(
    app: Option<tauri::AppHandle<R>>,
    call: Call,
    read: F,
) -> Result<Value, QueryError>
where
    R: tauri::Runtime,
    F: FnOnce(
        tauri::AppHandle<R>,
        crate::attribution::why_line::Params,
        String,
        crate::attribution::why_line::ReadHold,
    ) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let params = strict(call.args, query_args("whyLine"))?;
    let app = app.ok_or_else(|| QueryError::Failed("the app is not running".into()))?;
    let _hold = call.hold;
    let value = read(
        app,
        params,
        call.folder.to_string_lossy().into_owned(),
        _hold.clone(),
    )
    .await
    .map_err(|e| {
        if e.starts_with("invalid:") {
            QueryError::InvalidArgs(e)
        } else {
            QueryError::Failed(e)
        }
    })?;
    Ok(project_why_line(&value))
}

fn project_why_line(value: &Value) -> Value {
    if value["status"] == "unavailable" {
        return pick(value, &["status", "reason", "historyAsOf"], &[]);
    }
    let mut answer = pick(
        value,
        &["repo", "rev", "revSource", "resolvedCommit", "path", "notesRef", "historyAsOf"],
        &[],
    );
    if !answer["path"]
        .as_str()
        .is_some_and(crate::session_usage::story_changes::repo_relative)
    {
        answer["path"] = Value::Null;
    }
    answer["ranges"] = Value::Array(
        value["ranges"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|range| {
                let mut projected = pick(
                    range,
                    &[
                        "start",
                        "end",
                        "basis",
                        "commit",
                        "tool",
                        "model",
                        "sessionId",
                        "link",
                        "reason",
                        "story",
                        "candidateSessions",
                        "turn",
                        "unavailable",
                    ],
                    &[],
                );
                projected["turn"] = pick(
                    &range["turn"],
                    &["sessionId", "toolUseId", "turn", "basis", "taskId", "taskBasis", "reason"],
                    &[],
                );
                if let Some(unavailable) = range.get("unavailable") {
                    projected["unavailable"] = pick(unavailable, &["turn"], &[]);
                }
                projected
            })
            .collect(),
    );
    answer["sessions"] = Value::Object(
        value["sessions"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(id, facts)| (id.clone(), project_session(facts)))
            .collect(),
    );
    answer
}

/// An early `Busy` when a git read that outlived its answer is still running (the link's one
/// abandoned walk); a read inside its answer time never waits on another. The cap itself is
/// decided atomically when a read is abandoned (`Walk::drop`).
pub(super) fn admit_walk(
    walks: &Option<Arc<tokio::sync::Semaphore>>,
    needs_git: bool,
) -> Result<(), QueryError> {
    match walks {
        Some(walks) if needs_git && walks.available_permits() == 0 => Err(QueryError::Busy),
        _ => Ok(()),
    }
}

/// A hosted read running as its own task. Dropped before it ends (its answer timed out), it is
/// abandoned: it keeps the link's one abandoned-walk permit, and runs on to finish and keep its
/// git half, but only until its deadline (`Call::walk_deadline`, 120 s), when its cancel flag is
/// set and the walk stops at its next check. The task is never aborted: it holds the work's permits until
/// the blocking walk has really returned. A second abandoned read finds the permit taken and is
/// cancelled at once, so at most one runs on.
struct Walk {
    task: Option<tokio::task::JoinHandle<Result<Value, String>>>,
    walks: Option<Arc<tokio::sync::Semaphore>>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    deadline: std::time::Duration,
}

impl Walk {
    async fn finish(&mut self) -> Result<Result<Value, String>, tokio::task::JoinError> {
        let Some(task) = Option::as_mut(&mut self.task) else {
            return Ok(Err("the read already ended".into()));
        };
        let out = task.await;
        self.task = None;
        out
    }
}

impl Drop for Walk {
    fn drop(&mut self) {
        let Some(mut task) = Option::take(&mut self.task) else {
            return;
        };
        let (cancel, deadline) = (self.cancel.clone(), self.deadline);
        // The one atomic decision: the permit is taken here, once, at abandonment.
        let permit = Option::and_then(Option::as_ref(&self.walks), |walks| {
            Result::ok(tokio::sync::Semaphore::try_acquire_owned(walks.clone()))
        });
        if Option::is_some(&self.walks) && Option::is_none(&permit) {
            std::sync::atomic::AtomicBool::store(
                &cancel,
                true,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            std::sync::atomic::AtomicBool::store(
                &cancel,
                true,
                std::sync::atomic::Ordering::Relaxed,
            );
            return;
        };
        runtime.spawn(async move {
            tokio::select! {
                _ = &mut task => {}
                _ = tokio::time::sleep(deadline) => {
                    std::sync::atomic::AtomicBool::store(&cancel, true, std::sync::atomic::Ordering::Relaxed);
                    let _ = (&mut task).await;
                }
            }
            drop(permit);
        });
    }
}

fn story_changes(_ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QueryError>> {
    Box::pin(story_changes_via(call, |params, scope, cancel| {
        Box::pin(async move {
            crate::session_usage::story_changes::current_kept(&params, &scope, cancel).await
        })
    }))
}

/// The hosted `storyChanges`, with the read injected (the tests pass a slow one). The read gets
/// the cancel flag its walk checks between commits.
pub(super) async fn story_changes_via<F>(call: Call, read: F) -> Result<Value, QueryError>
where
    F: FnOnce(
            crate::session_usage::story_changes::Params,
            crate::context_repo::caller_scope::CallerScope,
            Arc<std::sync::atomic::AtomicBool>,
        ) -> BoxFuture<'static, Result<Value, String>>
        + Send
        + 'static,
{
    let params: crate::session_usage::story_changes::Params =
        strict(call.args, query_args("storyChanges"))?;
    let scope = crate::context_repo::caller_scope::for_content(&call.folder)
        .unwrap_or(crate::context_repo::caller_scope::CallerScope::Unknown);
    // SV-ENG-121: while a git read that outlived its answer still runs, another is `busy`; a
    // retry finds it done and its git half kept.
    let git = params.needs_git();
    admit_walk(&call.walks, git)?;
    // The read runs as its own task: when the answer times out it finishes (and keeps its
    // git half for the retry) instead of being dropped, holding the work's permit.
    let hold = call.hold;
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            let _hold = hold;
            read(params, scope, cancel).await
        }
    });
    let mut walk = Walk {
        task: Some(task),
        walks: if git { call.walks } else { None },
        cancel,
        deadline: call
            .walk_deadline
            .unwrap_or(std::time::Duration::from_secs(120)),
    };
    let value = walk
        .finish()
        .await
        .map_err(|_| QueryError::Failed("the read did not finish".into()))?
        .map_err(|e| {
            if e.starts_with("invalid:") {
                QueryError::InvalidArgs(e)
            } else {
                QueryError::Failed(e)
            }
        })?;
    Ok(project_story_changes(&value))
}

/// Hosted `storyTotals`: the index's answer with only the published reasons and no error text.
fn story_totals(_ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QueryError>> {
    Box::pin(async move {
        let params: crate::session_usage::story_totals::Params =
            strict(call.args, query_args("storyTotals"))?;
        let scope = crate::context_repo::caller_scope::for_content(&call.folder)
            .unwrap_or(crate::context_repo::caller_scope::CallerScope::Unknown);
        let home = crate::commands::paths::codesense_home_dir()
            .ok_or_else(|| QueryError::Failed("no CodeSense home directory".into()))?;
        blocking(call.hold, "live.storyTotals", move || {
            crate::session_usage::story_totals::read_blocking(&home, &params.story_ids, &scope, true)
        })
        .await
        .map_err(|e| {
            if e.starts_with("invalid:") {
                QueryError::InvalidArgs(e)
            } else {
                QueryError::Failed(e)
            }
        })
    })
}

/// What a hosted viewer may see of a `story_changes` answer; `totals` (counts only) pass whole.
fn project_story_changes(value: &Value) -> Value {
        let sessions: Vec<_> = value["sessions"].as_array().into_iter().flatten().map(project_session).collect();
        let tasks: Vec<_> = value["tasks"].as_array().into_iter().flatten().map(|t| json!({
            "taskId":t["taskId"],"parentId":t["parentId"],"reviews":t["reviews"]
        })).collect();
        let relative = |v: &Value| serde_json::Value::as_str(v).is_some_and(crate::session_usage::story_changes::repo_relative);
        let files = |v: &Value| v.as_array().into_iter().flatten().filter(|p| relative(p)).cloned().collect::<Vec<_>>();
        let commits: Vec<_> = value["commits"].as_array().into_iter().flatten().map(|c| json!({
            "sha":c["sha"],"authoredAt":c["authoredAt"],"files":files(&c["files"]),"sessionIds":c["sessionIds"],"basis":c["basis"],"unavailable":c["unavailable"]
        })).collect();
        let branches: Vec<_> = value["branches"].as_array().into_iter().flatten().map(|b|json!({
            "basis":b["basis"],"pr":pick(&b["pr"], &["number", "numberReason", "unavailable"], &[]),"prs":b["prs"],"availability":b["availability"],"mergedSha":b["mergedSha"],"unavailable":b["unavailable"]
        })).collect();
        let worktrees: Vec<_> = value["worktrees"].as_array().into_iter().flatten().map(|w|json!({"state":w["state"],"basis":w["basis"]})).collect();
        let prs: Vec<_> = value["prs"].as_array().into_iter().flatten().map(|pr|json!({
            "basis":pr["basis"],"number":pr["number"],"mergedSha":pr["mergedSha"],"mergedAt":pr["mergedAt"],"source":pr["source"],"commits":pr["commits"],"numberReason":pr["numberReason"]
        })).collect();
        let actual: Vec<_> = value["actual"].as_array().into_iter().flatten().filter(|a|relative(&a["path"])).map(|a|json!({
            "path":a["path"],"inPlanned":a["inPlanned"],"area":a["area"],"decision":a["decision"]
        })).collect();
        // Every published reason is named; per-file line reasons (fileLines) stay private with the counts.
        let unavailable = pick(&value["unavailable"], crate::session_usage::story_totals::PUBLISHED_REASONS, &[]);
        // The review-rounds warning is counts-only text, like the reasons.
        let warnings = pick(&value["warnings"], &["reviewRounds", "reviewTasks"], &[]);
        let reviews: Option<Vec<Value>> = value["reviews"].as_array().map(|rows| rows.iter().map(project_review).collect());
        let mut answer = json!({"storyId":value["storyId"],"sessions":sessions,"tasks":tasks,
            "totals":value["totals"],"unavailable":unavailable,"historyAsOf":value["historyAsOf"],"warnings":warnings,
            "commits":commits,"branches":branches,"worktrees":worktrees,"prs":prs,"total":value["total"],"nextCursor":value["nextCursor"],"scanLimit":value["scanLimit"],
            "plannedBase":if relative(&value["plannedBase"]){value["plannedBase"].clone()}else{Value::Null},
            "planned":if value["planned"].is_null(){Value::Null}else{json!(files(&value["planned"]))},"actual":actual,"plannedUntouched":files(&value["plannedUntouched"])});
        // An `only` read leaves the arrays it did not ask for out of the answer; so does this,
        // by the same keys the read's own pruning uses.
        use crate::session_usage::story_changes::{GIT_ONLY, OWNED};
        let owned = Iterator::flat_map(OWNED.iter(), |(_, keys)| keys.iter());
        for key in Iterator::chain(owned, GIT_ONLY.iter()) {
            if Option::is_none(&Value::get(value, *key)) {
                if let Some(map) = Value::as_object_mut(&mut answer) {
                    serde_json::Map::remove(map, *key);
                }
            }
        }
        if let Some(reviews) = reviews {
            answer["reviews"] = json!(reviews);
        }
        answer
}

/// A review round as a hosted viewer sees it: what the marker records of the round and its
/// findings, without the id (it can embed a PR ref), subject, reviewer, session, task or
/// finding ids.
fn project_review(row: &Value) -> Value {
    pick(row, &["round", "phase", "scope", "skill", "subjectRev",
        "high", "medium", "low", "dismissed", "converged", "reportedAt"], &[])
}

/// `session_usage`'s twin. Default 50 sessions, at most 500.
fn session_usage(ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QueryError>> {
    Box::pin(async move {
        let mut filter: crate::session_usage::Filter = strict(call.args, query_args("sessionUsage"))?;
        if filter.cwd.is_some() && !call.fields.iter().any(|f| f == "cwd") {
            return Err(QueryError::InvalidArgs(
                "cwd is not shared by this dashboard".into(),
            ));
        }
        if filter.story.is_some() && !call.fields.iter().any(|f| f == "story") {
            return Err(QueryError::InvalidArgs(
                "story is not shared by this dashboard".into(),
            ));
        }
        filter.limit = Some(capped(filter.limit));
        let launch_stories = ctx
            .app
            .as_ref()
            .map(crate::session_usage::launch_stories)
            .unwrap_or_default();
        let scope = if let Some(app) = ctx.app.as_ref() {
            let host = call
                .scope
                .as_ref()
                .map(|scope| format!("{}:{}", scope.org, scope.team));
            crate::context_repo::caller_scope::for_host(
                app,
                Some(&call.folder.to_string_lossy()),
                None,
                host.as_deref(),
            )
            .map_err(QueryError::Failed)?
        } else {
            crate::context_repo::caller_scope::for_content(&call.folder)
                .unwrap_or(crate::context_repo::caller_scope::CallerScope::Unknown)
        };
        let report = blocking(call.hold, "live.sessionUsage", move || {
            crate::session_usage::read_usage_for(&filter, launch_stories, &scope)
        })
        .await
        .map_err(|e| {
            if e.starts_with("invalid:") {
                QueryError::InvalidArgs(e)
            } else {
                QueryError::Failed(e)
            }
        })?;
        Ok(usage_answer(&report, &call.fields))
    })
}

/// A usage report as a dashboard gets it: each session cut to [`USAGE_SESSION_KEYS`] plus
/// the optional fields the manifest names (`cwd`, `story`).
fn usage_answer(usage: &crate::session_usage::UsageReport, fields: &[String]) -> Value {
    let report_session_count = usage.session_count;
    let report = json!(usage);
    let sessions: Vec<Value> = report["sessions"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|r| pick(r, USAGE_SESSION_KEYS, fields))
                .collect()
        })
        .unwrap_or_default();
    let mut answer = json!({"sessions": [], "days": report["days"], "totals": report["totals"]});
    if let Some(reason) = &usage.unavailable_reason {
        answer["unavailableReason"] = json!(reason);
    }
    let matched = report_session_count.max(sessions.len());
    let page = bounded_rows(sessions, matched, MAX_ROWS, &answer);
    let next = usage.cursor_after_kept(page.kept());
    page.put_page_in(&mut answer, "sessions", next);
    answer
}

const SESSION_KEYS: &[&str] = &[
    "handle",
    "displayName",
    "state",
    "launchedBy",
    "story",
    "role",
    "avatar",
    "agent",
    "machine",
    "unread",
    "deadlineAtMs",
    "expiring",
];

/// `list_sessions`'s twin. The whole read runs on a blocking thread that holds
/// the slot, so a dropped query cannot leave work behind it.
fn sessions(ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QueryError>> {
    Box::pin(async move {
        let params: crate::mcp::handler::ListSessionsParams = strict(call.args, query_args("sessions"))?;
        let app = ctx
            .app
            .ok_or_else(|| QueryError::Failed("the app is not running".into()))?;
        let handle = tokio::runtime::Handle::current();
        let rows = blocking(call.hold, "live.sessions", move || {
            handle.block_on(crate::mcp::session_list::read_session_list(
                &app,
                params.launched_by.as_deref(),
            ))
        })
        .await
        .map_err(QueryError::Failed)?;
        Ok(sessions_answer(&rows, &call.fields))
    })
}

/// The `sessions` answer for `rows`.
fn sessions_answer(rows: &[crate::mcp::session_list::SessionListRow], fields: &[String]) -> Value {
    let mut answer = json!({ "sessions": [] });
    let values = rows.iter().map(|row| session_value(row, fields));
    bounded_rows(values, rows.len(), MAX_ROWS, &answer).put_in(&mut answer, "sessions");
    answer
}

/// SW-DP-21: one session row as a dashboard gets it, for the `sessions` query
/// and the `terminals` stream alike, so the two cannot drift: [`SESSION_KEYS`]
/// plus the optional fields the manifest names (`workspace`, `progressNote`).
pub(crate) fn session_value(
    row: &crate::mcp::session_list::SessionListRow,
    fields: &[String],
) -> Value {
    let note = fields.iter().any(|f| f == "progressNote");
    let full = json!(row);
    let mut out = pick(&full, SESSION_KEYS, fields);
    if let Some(progress) = full.get("progress") {
        let keep: &[&str] = if note {
            &["phase", "step", "total", "atMs", "note"]
        } else {
            &["phase", "step", "total", "atMs"]
        };
        out["progress"] = pick(progress, keep, &[]);
    }
    out
}

const RUN_OUTCOME_KEYS: &[&str] = &[
    "story_id",
    "run_id",
    "status",
    "exit_code",
    "started_at",
    "ended_at",
    "source",
    "automation_id",
    "trigger_source",
    "scheduled_at",
    "fired_at",
];

/// `list_runs`'s twin, as `{runs}`. Default 50 runs, at most 500.
fn runs(_ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QueryError>> {
    Box::pin(async move {
        let params: crate::mcp::run_tools::ListRunsParams = strict(call.args, query_args("runs"))?;
        let cap = runs_cap(params.limit);
        let handle = tokio::runtime::Handle::current();
        let runs = blocking(call.hold, "live.runs", move || {
            handle.block_on(crate::commands::sprint_run::run_store::list_run_outcomes())
        })
        .await
        .map_err(QueryError::Failed)?;
        Ok(runs_answer(&runs, cap, &call.fields))
    })
}

/// The rows a `runs` call may return: 0 reads as unset, as it does for `list_runs`.
fn runs_cap(limit: Option<u32>) -> usize {
    capped(limit.filter(|n| *n > 0).map(|n| n as usize))
}

/// The `runs` answer for `runs` (newest first).
fn runs_answer(
    runs: &[crate::commands::sprint_run::run_store::RunOutcomeRead],
    cap: usize,
    fields: &[String],
) -> Value {
    let rows = runs.iter().map(|run| {
        let full = json!(run);
        json!({
            "outcome": pick(&full["outcome"], RUN_OUTCOME_KEYS, fields),
            "validated": full["validated"],
            "has_structured_transcript": full["has_structured_transcript"],
        })
    });
    let mut answer = json!({ "runs": [] });
    bounded_rows(rows, runs.len(), cap, &answer).put_in(&mut answer, "runs");
    answer
}

/// The viewer's part of a `queryContext` call: narrowing only.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextArgs {
    /// Equality on allowed fields: `{status: "todo"}` or `{status: ["todo", "review"]}`.
    #[serde(default, rename = "match")]
    pub match_fields: BTreeMap<String, Value>,
    /// Allowed fields, `-` first for descending.
    #[serde(default)]
    pub sort: Vec<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Count rows per value of an allowed field.
    #[serde(default)]
    pub group_by: Option<String>,
    /// The local SDK's shape (SV-INP-29), narrowing only: `where.kinds` picks
    /// content kinds (the team and layer stay fixed to the dashboard's).
    #[serde(default, rename = "where")]
    pub where_scope: Option<ContextWhere>,
    /// The local SDK's `filters`, `eq` and `in` on allowed fields only.
    #[serde(default)]
    pub filters: Vec<ContextFilter>,
    /// The fields to return, a subset of the ones the dashboard shares.
    #[serde(default)]
    pub select: Option<Vec<String>>,
    /// The `nextCursor` of the previous page, with the same other args.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextWhere {
    #[serde(default)]
    pub kinds: Vec<crate::context_query::model::Kind>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextFilter {
    pub key: String,
    pub op: String,
    pub value: Value,
}

/// The engine query for a dashboard in `org`/`team` that allows `fields`, or
/// why the viewer's args ask for more.
pub fn context_query(
    args: Value,
    fields: &[String],
    org: &str,
    team: &str,
) -> Result<crate::context_query::ContextQuery, QueryError> {
    let args: ContextArgs = strict(args, query_args("queryContext"))?;
    let allowed = |key: &str| fields.iter().any(|f| f == key);
    for (key, value) in &args.match_fields {
        if !allowed(key) {
            return Err(QueryError::InvalidArgs(format!(
                "{key} is not shared by this dashboard"
            )));
        }
        let scalar = |v: &Value| v.is_string() || v.is_number() || v.is_boolean();
        let ok = scalar(value)
            || value
                .as_array()
                .is_some_and(|a| a.len() <= 50 && a.iter().all(scalar));
        if !ok {
            return Err(QueryError::InvalidArgs(format!(
                "{key} must be a value or a list of values"
            )));
        }
    }
    let mut filters = Vec::new();
    for clause in &args.filters {
        if !allowed(&clause.key) {
            return Err(QueryError::InvalidArgs(format!(
                "{} is not shared by this dashboard",
                clause.key
            )));
        }
        let scalar = |v: &Value| v.is_string() || v.is_number() || v.is_boolean();
        let ok = match clause.op.as_str() {
            "eq" => scalar(&clause.value),
            "in" => clause
                .value
                .as_array()
                .is_some_and(|a| a.len() <= 50 && a.iter().all(scalar)),
            other => {
                return Err(QueryError::InvalidArgs(format!(
                    "filter op {other} is not available here (eq and in only)"
                )))
            }
        };
        if !ok || args.match_fields.contains_key(&clause.key) {
            return Err(QueryError::InvalidArgs(format!(
                "filter on {} needs a value (a list for in) and no match on the same key",
                clause.key
            )));
        }
        filters.push(json!({"key": clause.key, "op": clause.op, "value": clause.value}));
    }
    if filters.len() > 16 {
        return Err(QueryError::InvalidArgs("more than 16 filters".into()));
    }
    let select = match &args.select {
        None => fields.to_vec(),
        Some(list) => {
            if let Some(field) = list.iter().find(|f| !allowed(f)) {
                return Err(QueryError::InvalidArgs(format!(
                    "{field} is not shared by this dashboard"
                )));
            }
            list.clone()
        }
    };
    let kinds = args
        .where_scope
        .as_ref()
        .map(|w| w.kinds.clone())
        .filter(|k| !k.is_empty());
    let mut sort = Vec::new();
    for key in &args.sort {
        let field = key.strip_prefix('-').unwrap_or(key);
        if !allowed(field) && field != "mtime" {
            return Err(QueryError::InvalidArgs(format!("cannot sort by {field}")));
        }
        sort.push(Value::String(key.clone()));
    }
    if let Some(group) = &args.group_by {
        if !allowed(group) {
            return Err(QueryError::InvalidArgs(format!("cannot group by {group}")));
        }
    }
    let query = json!({
        "where": {"orgs": [org], "teams": [team], "layers": ["team"], "kinds": kinds},
        "match": args.match_fields,
        "filters": filters,
        "select": select,
        "sort": sort,
        "limit": capped(args.limit.or(Some(MAX_ROWS))),
        "groupBy": args.group_by,
        "cursor": args.cursor,
    });
    serde_json::from_value(query).map_err(|e| QueryError::InvalidArgs(e.to_string()))
}

/// The team content root holding `folder` (deepest wins): its org and team.
pub fn team_of(
    roots: &[crate::context_query::index::IndexRoot],
    folder: &std::path::Path,
) -> Option<(String, String)> {
    let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    roots
        .iter()
        .filter(|r| r.layer == crate::context_query::model::Layer::Team)
        .filter(|r| {
            let dir = std::fs::canonicalize(&r.dir).unwrap_or_else(|_| r.dir.clone());
            folder.starts_with(&dir)
        })
        .max_by_key(|r| r.dir.components().count())
        .and_then(|r| Some((r.org.clone()?, r.team.clone()?)))
}

/// One row as a viewer gets it: no absolute path, no summary, no relationships,
/// only the allowed frontmatter fields.
pub fn context_row(row: &crate::context_query::model::ContextRow, fields: &[String]) -> Value {
    let picked: Map<String, Value> = row
        .fields
        .iter()
        .filter(|(k, _)| fields.iter().any(|f| f == *k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut out = json!({
        "id": row.id,
        "relPath": row.rel_path,
        "fileName": row.file_name,
        "kind": row.kind,
        "mtime": row.mtime,
        "fields": picked,
    });
    if fields.iter().any(|f| f == "title") {
        out["title"] = json!(row.title);
    }
    out
}

/// `query_context`'s twin, fixed to the dashboard's own org and team.
fn query_context(ctx: LiveCtx, call: Call) -> BoxFuture<'static, Result<Value, QueryError>> {
    Box::pin(async move {
        let service = ctx
            .context_service()
            .ok_or_else(|| QueryError::Failed("the context index is not running".into()))?;
        let (org, team) = check_scope(
            call.scope.as_ref(),
            team_of(&service.snapshot().roots, &call.folder),
        )?;
        let query = context_query(call.args, &call.fields, &org, &team)?;
        let viewer = crate::context_query::Viewer {
            page: Some(call.folder.display().to_string()),
            ..Default::default()
        };
        let fields = query.select.clone().unwrap_or_else(|| call.fields.clone());
        let (run_service, run_query, run_viewer) = (service.clone(), query.clone(), viewer.clone());
        let result = blocking(call.hold, "live.queryContext", move || {
            Ok(crate::context_query::run_query(
                &run_service,
                &run_query,
                &run_viewer,
            ))
        })
        .await
        .map_err(QueryError::Failed)?
        .map_err(|e| match e {
            crate::context_query::QueryError::StaleCursor => QueryError::StaleCursor,
            crate::context_query::QueryError::InvalidCursor(e) => {
                QueryError::InvalidArgs(format!("invalid: {e}"))
            }
            crate::context_query::QueryError::Invalid(e) => QueryError::Failed(e),
        })?;
        let rows: Vec<Value> = result
            .rows
            .iter()
            .map(|r| context_row(r, &fields))
            .collect();
        Ok(context_answer(
            result.total,
            rows,
            json!(result.groups),
            result.next_cursor.clone(),
            |kept| {
                let next = crate::context_query::cursor_after_kept(&query, &viewer, &result, kept);
                if next.is_some() && query.cursor.is_none() {
                    service.keep_walk(result.generation);
                }
                next
            },
        ))
    })
}

/// The `queryContext` answer for the page `rows` out of `total` matches. `engine_next` is the
/// engine's cursor after the whole page; `after_kept` gives the cursor after the first n rows, for
/// a byte cut. `nextCursor` points after the last row sent, so a cut loses no row.
fn context_answer(
    total: usize,
    rows: Vec<Value>,
    groups: Value,
    engine_next: Option<String>,
    after_kept: impl FnOnce(usize) -> Option<String>,
) -> Value {
    let mut answer = json!({
        "total": total,
        "truncated": false,
        "rows": [],
        "groups": groups,
    });
    let page_len = rows.len();
    let page = bounded_rows(rows, total, MAX_ROWS, &answer);
    let next = if page.kept() < page_len {
        after_kept(page.kept())
    } else {
        engine_next
    };
    page.put_page_in(&mut answer, "rows", next);
    answer
}

/// Token shapes `redact_secrets` does not cover: publisher, viewer and session
/// tokens (L2: `csp_…`, `cvs_…`, `csb_…`) and Anthropic keys (`sk-ant-…`).
static EXTRA_TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:csp|cvs|csb)_[A-Za-z0-9_\-]+|sk-ant-[A-Za-z0-9_\-]{10,}")
        .expect("token pattern compiles")
});

pub(crate) fn redact_str(text: &str) -> String {
    let text = EXTRA_TOKENS.replace_all(text, REDACTION_PLACEHOLDER);
    redact_secrets(&text)
}

/// [`EXTRA_TOKENS`], for the live terminal's streaming scrub.
pub(crate) fn extra_tokens() -> &'static Regex {
    &EXTRA_TOKENS
}

/// The words of an object key, lowercased. It splits at separators, at a
/// lower-to-upper change (`apiKey`) and where an acronym meets a word
/// (`APIToken` is `api`, `token`; `JWTPassword` is `jwt`, `password`).
fn key_words(key: &str) -> Vec<String> {
    let chars: Vec<char> = key.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        if c.is_ascii_uppercase() && !current.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower)
            {
                words.push(std::mem::take(&mut current));
            }
        }
        current.push(c.to_ascii_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// True when an object key names a credential. The key is split into words
/// ([`key_words`]), so `author`, `tokenizer` or `keystone` do not match.
/// `key` alone is not sensitive (a sort key, a React key); it is after
/// access, signing, encryption, session, secret, private or api.
pub fn sensitive_key(key: &str) -> bool {
    const WORDS: &[&str] = &[
        "password",
        "passwords",
        "passwd",
        "pwd",
        "passphrase",
        "secret",
        "secrets",
        "token",
        "tokens",
        "apikey",
        "auth",
        "authorization",
        "credential",
        "credentials",
        "privatekey",
        "cookie",
        "cookies",
        "bearer",
        "dsn",
        "databaseurl",
    ];
    const KEY_AFTER: &[&str] = &[
        "access",
        "signing",
        "encryption",
        "session",
        "secret",
        "private",
        "api",
    ];
    let words = key_words(key);
    words.iter().any(|w| WORDS.contains(&w.as_str()))
        || words.windows(2).any(|p| {
            (KEY_AFTER.contains(&p[0].as_str()) && (p[1] == "key" || p[1] == "keys"))
                || (p[0] == "database" && p[1] == "url")
        })
}

/// Under a credential key every string is replaced; numbers, booleans and
/// nulls are kept (token *counts* are not secrets).
fn blank_strings(value: Value) -> Value {
    match value {
        Value::String(_) => Value::String(REDACTION_PLACEHOLDER.into()),
        Value::Array(items) => Value::Array(items.into_iter().map(blank_strings).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (redact_str(&k), blank_strings(v)))
                .collect(),
        ),
        other => other,
    }
}

/// DP-INV-10: every string in an answer, keys included, with secrets replaced,
/// and every string under a credential-named key replaced whatever it holds.
pub fn redact(value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(redact_str(&s)),
        Value::Array(items) => Value::Array(items.into_iter().map(redact).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let v = if sensitive_key(&k) {
                        blank_strings(v)
                    } else {
                        redact(v)
                    };
                    (redact_str(&k), v)
                })
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fixture runner that answers `{"fixture": true}`.
    pub(crate) fn fixture_run(
        _ctx: LiveCtx,
        _call: Call,
    ) -> BoxFuture<'static, Result<Value, QueryError>> {
        Box::pin(async { Ok(json!({"fixture": true})) })
    }

    #[test]
    fn hosted_why_line_and_story_changes_share_history_facts() {
        let as_of = json!({"fetchedAt": 1, "originHead": "h"});
        let why = project_why_line(&json!({"repo": "a/b", "rev": "x", "revSource": "mergedSha",
            "resolvedCommit": "x", "path": "a.txt", "notesRef": null, "historyAsOf": as_of,
            "ranges": [], "sessions": {}}));
        assert_eq!(why["revSource"], "mergedSha");
        assert_eq!(why["historyAsOf"], as_of);
        let unavailable = project_why_line(&json!({"status": "unavailable", "reason": "r", "historyAsOf": as_of}));
        assert_eq!(unavailable["historyAsOf"], as_of);
        let story = project_story_changes(&json!({"historyAsOf": as_of,
            "unavailable": {"historyFetch": "fetch timed out"}}));
        assert_eq!(story["historyAsOf"], as_of);
        assert_eq!(story["unavailable"]["historyFetch"], "fetch timed out");
    }

    /// SV-CDE-64: the hosted projection carries the line shares (counts only) and `mergedAt`.
    #[test]
    fn hosted_story_changes_shares_line_totals_and_merged_at() {
        let lines = json!({"ai":{"total":5,"byRuntime":{"claude":3,"codex":2,"opencode":0,"other":0}},
            "human":1,"unattributed":2,"total":8});
        let answer = project_story_changes(&json!({
            "storyId":"SV-1","totals":{"commits":2,"lines":lines},
            "unavailable":{},"prs":[{"number":7,"mergedSha":"abc","mergedAt":1_300_000,"source":"squash-subject","basis":"associated","branch":"secret"}]
        }));
        assert_eq!(answer["totals"]["lines"], lines);
        assert_eq!(answer["prs"][0]["mergedAt"], 1_300_000);
        assert!(answer["prs"][0].get("branch").is_none());
        let none = project_story_changes(&json!({"totals":{"lines":null},"unavailable":{"lines":"why"}}));
        assert!(none["totals"]["lines"].is_null());
        assert_eq!(none["unavailable"]["lines"], "why");
    }

    /// A runtime bucket the host adds later (a newly listed CLI) reaches the hosted answer untouched.
    #[test]
    fn hosted_story_changes_passes_every_runtime_bucket_through() {
        let lines = json!({"ai":{"total":7,"byRuntime":{"claude":3,"codex":2,"opencode":0,"hermes":2,"other":0}},
            "human":0,"unattributed":0,"total":7});
        let answer = project_story_changes(&json!({"storyId":"SV-1","totals":{"commits":1,"lines":lines},"unavailable":{}}));
        assert_eq!(answer["totals"]["lines"]["ai"]["byRuntime"]["hermes"], 2);
        assert_eq!(answer["totals"]["lines"], lines);
    }

    #[test]
    fn hosted_story_changes_keeps_file_line_reasons_private() {
        let answer = project_story_changes(&json!({"unavailable":{"fileLines":"binary","lines":"why","costUsd":"unpriced","unnamed":"x"}}));
        assert_eq!(answer["unavailable"], json!({"lines":"why","costUsd":"unpriced"}));
    }

    /// SV-CDE-91 AC5: `only` is a published storyChanges argument (`include` stays agent-only),
    /// `sessionRead` is not a published query (Turns open in the app), and an `only` answer
    /// leaves out what it did not read instead of answering it empty.
    #[test]
    fn hosted_story_changes_takes_only_and_projects_what_it_returns() {
        assert!(query_args("storyChanges").contains(&"only"));
        assert!(!query_args("storyChanges").contains(&"include"));
        assert!(find(CATALOG, "sessionRead").is_none());
        // `only` is a list argument: a manifest may pin it, and every item must pass the pin.
        assert!(LIST_ARGS.contains(&("storyChanges", "only")));
        use crate::dashboard_publish::manifest::{pin_allows as pin_allows_for, LiveArgs, Pin};
        let pins: LiveArgs = [(
            "storyChanges".to_owned(),
            [("only".to_owned(), vec![Pin::String("tasks".into()), Pin::String("reviews".into())])].into(),
        )]
        .into();
        let allows = |args: Value| pin_allows_for(&pins, "storyChanges", &args, |_, _| false);
        assert!(allows(json!({"only":["tasks"]})) && allows(json!({"only":["tasks","reviews"]})));
        assert!(!allows(json!({"only":["tasks","commits"]})), "an item outside the pin");
        assert!(!allows(json!({"only":[]})) && !allows(json!({"only":"tasks"})) && !allows(json!({})));
        let strict_only: Result<crate::session_usage::story_changes::Params, _> =
            strict(json!({"storyId":"SV-1","only":["tasks","reviews"]}), query_args("storyChanges"));
        assert!(strict_only.is_ok());
        let rejected: Result<crate::session_usage::story_changes::Params, _> =
            strict(json!({"storyId":"SV-1","include":["ranges"]}), query_args("storyChanges"));
        assert!(rejected.is_err());
        let answer = project_story_changes(&json!({
            "storyId":"SV-1","tasks":[{"taskId":"t","parentId":null,"reviews":[],"goal":"private goal"}],
            "totals":{"sessions":1},"unavailable":{"commits":"not read","lines":"not read"},"warnings":{},
            "reviews":[{"id":"r1","round":1,"phase":"review","scope":"full","skill":"bmad-code-review",
                "subjectKind":"pr","subjectRef":"PR #1","subjectRev":"abc","high":0,"medium":1,"low":2,
                "dismissed":0,"converged":false,"reportedAt":5,"reviewer":"someone","findingIds":"f1"}],
        }));
        assert_eq!(answer["tasks"], json!([{"taskId":"t","parentId":null,"reviews":[]}]));
        // The reasons of an `only` read reach hosted: files and reviews are published reasons.
        let reasons = project_story_changes(&json!({"unavailable":{"files":"not read","reviews":"no team","commits":"c","lines":"l","fileLines":"private"}}));
        assert_eq!(reasons["unavailable"], json!({"files":"not read","reviews":"no team","commits":"c","lines":"l"}));
        assert_eq!(answer["reviews"][0], json!({"round":1,"phase":"review","scope":"full",
            "skill":"bmad-code-review","subjectRev":"abc","high":0,"medium":1,"low":2,"dismissed":0,
            "converged":false,"reportedAt":5}));
        for key in ["sessions", "commits", "branches", "worktrees", "prs", "actual", "planned",
            "plannedUntouched", "plannedBase", "total", "nextCursor", "scanLimit", "historyAsOf"] {
            assert!(answer.get(key).is_none(), "{key} answered though it was not read");
        }
        // A read of every array keeps every key, even where the value is null.
        let full = project_story_changes(&json!({"storyId":"SV-1","sessions":[],"tasks":[],"commits":[],
            "branches":[],"worktrees":[],"prs":[],"actual":[],"planned":null,"plannedUntouched":[],
            "plannedBase":null,"total":0,"nextCursor":null,"scanLimit":2000,"historyAsOf":null,
            "totals":{},"unavailable":{},"warnings":{}}));
        for key in ["sessions", "commits", "prs", "actual", "planned", "total", "nextCursor", "historyAsOf"] {
            assert!(full.get(key).is_some(), "{key} dropped from a full read");
        }
        assert!(full.get("reviews").is_none());
    }

    /// SV-PBF-297: an associated `reviewTasks` count stays marked as associated when published,
    /// and its null reason is published with it.
    #[test]
    fn hosted_story_changes_carries_the_review_task_warning_and_reasons() {
        let answer = project_story_changes(&json!({
            "warnings":{"reviewRounds":"x","reviewTasks":"associated","other":"private"},
            "unavailable":{"reviewTasks":"why","branches":"side"}
        }));
        assert_eq!(answer["warnings"], json!({"reviewRounds":"x","reviewTasks":"associated"}));
        assert_eq!(answer["unavailable"], json!({"reviewTasks":"why","branches":"side"}));
    }

    fn call(args: Value) -> Call {
        Call {
            args,
            ..Default::default()
        }
    }

    #[test]
    fn every_entry_has_a_p11_name_a_description_and_says_what_it_shares() {
        let name = Regex::new(r"^[a-zA-Z][a-zA-Z0-9]{0,39}$").unwrap();
        let mut seen = std::collections::HashSet::new();
        for q in CATALOG {
            assert!(name.is_match(q.name), "{}", q.name);
            assert!(
                !q.describe.trim().is_empty(),
                "{} has no description",
                q.name
            );
            assert!(
                !q.shares.trim().is_empty(),
                "{} does not say what it shares",
                q.name
            );
            assert!(seen.insert(q.name), "{} is listed twice", q.name);
        }
        let names: Vec<_> = CATALOG.iter().map(|q| q.name).collect();
        assert_eq!(
            names,
            [
                "inbox",
                "media",
                "storyChanges",
                "storyTotals",
                "whyLine",
                "setup",
                "sessionUsage",
                "sessions",
                "runs",
                "queryContext"
            ]
        );
    }

    /// SW-DP-5 AC-2: per entry, bad args are `invalid_args` before the twin
    /// runs (no store is touched).
    #[tokio::test]
    async fn bad_args_are_invalid_for_every_entry() {
        let bad = [
            ("sessionUsage", json!({"sinceMs": "yesterday"})),
            ("sessionUsage", json!({"since": 1})),
            ("sessionUsage", json!({"cwd": "/Users/me"})),
            ("sessions", json!({"launchedBy": 7})),
            ("sessions", json!({"handle": "x"})),
            ("runs", json!({"limit": "ten"})),
            ("runs", json!({"limit": 5, "all": true})),
            ("runs", json!([1, 2])),
        ];
        for (name, args) in bad {
            let q = find(CATALOG, name).unwrap();
            let got = (q.run)(LiveCtx::default(), call(args.clone())).await;
            assert!(
                matches!(got, Err(QueryError::InvalidArgs(_))),
                "{name} {args}: {got:?}"
            );
        }
    }

    /// SW-DP-5 AC-2: args parse into the twin's own params struct, the same
    /// value the MCP handler receives for that input.
    #[test]
    fn good_args_parse_into_the_twins_params() {
        let usage = json!({"sinceMs": 1, "untilMs": 2, "sessionId": "s", "limit": 3});
        let ours: crate::session_usage::Filter = strict(usage.clone(), query_args("sessionUsage")).unwrap();
        let mcp: crate::session_usage::Filter = serde_json::from_value(usage).unwrap();
        assert_eq!(format!("{ours:?}"), format!("{mcp:?}"));

        let sessions = json!({"launchedBy": "h1"});
        let ours: crate::mcp::handler::ListSessionsParams =
            strict(sessions.clone(), query_args("sessions")).unwrap();
        let mcp: crate::mcp::handler::ListSessionsParams =
            serde_json::from_value(sessions).unwrap();
        assert_eq!(format!("{ours:?}"), format!("{mcp:?}"));

        let runs = json!({"limit": 20});
        let ours: crate::mcp::run_tools::ListRunsParams = strict(runs.clone(), query_args("runs")).unwrap();
        let mcp: crate::mcp::run_tools::ListRunsParams = serde_json::from_value(runs).unwrap();
        assert_eq!(format!("{ours:?}"), format!("{mcp:?}"));
        assert_eq!(capped(None), DEFAULT_ROWS);
        assert_eq!(capped(Some(10_000)), MAX_ROWS);
    }

    fn fields(list: &[&str]) -> Vec<String> {
        list.iter().map(|f| f.to_string()).collect()
    }

    fn usage_report() -> crate::session_usage::UsageReport {
        use crate::session_usage::{
            event::Tokens, SessionUsage, UsageReport, UsageStory, UsageTotals,
        };
        let session = |id: &str, story: Option<UsageStory>| SessionUsage {
            cli: codesense_pty_core::agent_cli::AgentCli::Claude,
            session_id: id.into(),
            org: Some("acme".into()),
            team: Some("alpha".into()),
            agent: Some("builtin/claude".into()),
            machine_id: "m1".into(),
            cwd: Some("/Users/me/codesense-wt/feat-SV-5".into()),
            story,
            models: vec!["claude-opus-5".into()],
            first_at_ms: 1,
            last_at_ms: 2,
            messages: 3,
            tokens: Tokens::default(),
            cost_usd: Some(0.5),
            unpriced_models: vec![],
            run_id: None,
        };
        UsageReport {
            sessions: vec![
                session(
                    "s1",
                    Some(UsageStory {
                        id: "SV-5".into(),
                        title: Some("Redesign".into()),
                        status: Some("todo".into()),
                        epic: None,
                        basis: None,
                    }),
                ),
                session("s2", None),
            ],
            days: vec![],
            totals: UsageTotals {
                tokens: Tokens::default(),
                cost_usd: 1.0,
                priced_sessions: 2,
                unpriced_sessions: 0,
            },
            session_count: 2,
            next_cursor: None,
            unavailable_reason: None,
            cursor_key: 0,
        }
    }

    fn heavy_session(i: usize) -> crate::session_usage::SessionUsage {
        use crate::session_usage::{event::Tokens, SessionUsage};
        SessionUsage {
            cli: codesense_pty_core::agent_cli::AgentCli::Claude,
            session_id: format!("0b9c2f1e-4d7a-4c1b-9e55-{i:012}"),
            org: Some("riverside".into()),
            team: Some("riverside-engineering".into()),
            agent: Some("builtin/claude".into()),
            machine_id: "m1".into(),
            cwd: None,
            story: None,
            models: (0..8)
                .map(|m| format!("claude-opus-5-5-2026-10-0{m}"))
                .collect(),
            first_at_ms: 1_700_000_000_000 + i as i64,
            last_at_ms: 1_700_000_900_000 + i as i64,
            messages: 3,
            tokens: Tokens::default(),
            cost_usd: Some(0.5),
            unpriced_models: vec![
                "claude-haiku-5-5-2026-10-01".into(),
                "claude-sonnet-5-5-2026-10-02".into(),
            ],
            run_id: None,
        }
    }

    /// SV-INP-38: 500 sessions are over the hosted cap whole; the Mac cuts the list newest
    /// first to fit, flags it, and the totals still cover every session.
    #[test]
    fn a_usage_answer_too_big_for_the_hosted_cap_is_cut_and_flagged_never_failed() {
        let mut report = usage_report();
        report.sessions = (0..500).rev().map(heavy_session).collect();
        report.totals.priced_sessions = 500;
        report.session_count = 500;
        let whole =
            json!({"sessions": report.sessions.iter().map(|s| json!(s)).collect::<Vec<_>>()});
        assert!(
            serde_json::to_vec(&whole).unwrap().len() > super::super::live::MAX_ANSWER_BYTES,
            "the premise: 500 rows do not fit"
        );
        let answer = usage_answer(&report, &[]);
        let rows = answer["sessions"].as_array().unwrap();
        assert!(serde_json::to_vec(&answer).unwrap().len() <= super::super::live::MAX_ANSWER_BYTES);
        assert!(rows.len() < 500 && !rows.is_empty());
        assert_eq!(answer["truncated"], true);
        assert_eq!(answer["returned"], rows.len());
        assert_eq!(
            rows[0]["sessionId"], report.sessions[0].session_id,
            "newest first"
        );
        assert_eq!(answer["totals"]["pricedSessions"], 500, "totals cover all");
    }

    fn run_read(
        i: usize,
        story_id: &str,
    ) -> crate::commands::sprint_run::run_store::RunOutcomeRead {
        serde_json::from_value(json!({
            "outcome": {
                "story_id": story_id,
                "run_id": format!("r{i}"),
                "status": "success",
                "ended_at": "2026-10-04T10:00:00Z",
                "source": "signal"
            },
            "validated": "completed"
        }))
        .expect("a run outcome")
    }

    fn assert_marked_cut(answer: &Value, key: &str) -> usize {
        let rows = answer[key].as_array().unwrap().len();
        assert_eq!(answer["truncated"], true, "{key}: {answer}");
        assert_eq!(
            answer["returned"], rows,
            "{key}: truncated implies returned"
        );
        rows
    }

    /// SV-ENG-61 AC-1: a `runs` list cut at the 50 default says so, and how many it holds.
    #[test]
    fn runs_cut_at_the_limit_say_so() {
        let runs: Vec<_> = (0..120).map(|i| run_read(i, "SW-1")).collect();
        let answer = runs_answer(&runs, capped(None), &[]);
        assert_eq!(assert_marked_cut(&answer, "runs"), DEFAULT_ROWS);
        assert_eq!(answer["total"], 120);
        let answer = runs_answer(&runs[..10], capped(None), &[]);
        assert_eq!(answer["runs"].as_array().unwrap().len(), 10);
        assert!(answer.get("truncated").is_none() && answer.get("returned").is_none());
    }

    /// SV-ENG-61 AC-1/AC-4: runs too big for the hosted answer cap are cut by bytes and flagged.
    #[test]
    fn runs_cut_by_size_say_so() {
        let big = "x".repeat(8 * 1024);
        let runs: Vec<_> = (0..50).map(|i| run_read(i, &big)).collect();
        let answer = runs_answer(&runs, capped(None), &[]);
        let rows = assert_marked_cut(&answer, "runs");
        assert!(rows > 0 && rows < 50);
        assert!(serde_json::to_vec(&answer).unwrap().len() <= super::super::live::MAX_ANSWER_BYTES);
    }

    /// SV-ENG-61 AC-2: `limit` 1 against 3 sessions is a limit cut; the totals still cover all 3.
    #[test]
    fn a_usage_answer_cut_at_the_limit_says_so_and_totals_cover_all() {
        use crate::session_usage::tests::{event, local_noon, store};
        let (_dir, mut store) = store();
        let tokens = || crate::session_usage::event::Tokens {
            input: 1,
            ..Default::default()
        };
        let events: Vec<_> = (0..3)
            .map(|i| {
                event(
                    &format!("req_{i}"),
                    &format!("s{i}"),
                    "claude-sonnet-4-6",
                    local_noon(2026, 10, 15 + i),
                    tokens(),
                )
            })
            .collect();
        store.ingest(&events).unwrap();
        let filter = crate::session_usage::Filter {
            limit: Some(1),
            ..Default::default()
        };
        let report = store.report(&filter).unwrap();
        let report_json = serde_json::to_value(&report).unwrap();
        let answer = usage_answer(&report, &[]);
        assert_eq!(assert_marked_cut(&answer, "sessions"), 1);
        assert_eq!(answer["total"], 3);
        assert_eq!(
            report_json["total"], answer["total"],
            "hosted total matches the serialized report total"
        );
        assert!(
            report_json.get("session_count").is_none(),
            "the internal field name is not served"
        );
        assert_eq!(answer["totals"]["pricedSessions"], 3, "totals cover all");
        let whole = store.report(&Default::default()).unwrap();
        let whole = usage_answer(&whole, &[]);
        assert_eq!(whole["truncated"], false);
        assert_eq!(
            (&whole["returned"], &whole["total"]),
            (&json!(3), &json!(3))
        );
        assert!(whole.get("nextCursor").is_none());
    }

    /// SV-ENG-62 AC-1/AC-3: 600 sessions are counted before the 500 cap, and the cut is flagged.
    #[test]
    fn sessions_over_the_cap_are_counted_first_and_flagged() {
        use crate::dashboard_publish::agents::tests::row;
        use crate::mcp::session_list::SessionState;
        let rows: Vec<_> = (0..600)
            .map(|i| {
                row(
                    &format!("h{i:04}"),
                    SessionState::Running,
                    "This Mac",
                    None,
                    None,
                    None,
                )
            })
            .collect();
        let answer = sessions_answer(&rows, &[]);
        let kept = assert_marked_cut(&answer, "sessions");
        assert!(kept <= MAX_ROWS);
        assert_eq!(answer["total"], 600);
        assert_eq!(answer["sessions"][0]["handle"], "h0000", "handle order");
        let answer = sessions_answer(&rows[..3], &[]);
        assert!(answer.get("truncated").is_none() && answer.get("total").is_none());
    }

    /// SV-ENG-61: a `queryContext` limit cut carries `returned` like every other hosted cut.
    #[test]
    fn a_context_limit_cut_carries_returned() {
        let rows: Vec<Value> = (0..50).map(|i| json!({"i": i})).collect();
        let answer = context_answer(120, rows, json!([]), Some("next".into()), |_| None);
        assert_eq!(assert_marked_cut(&answer, "rows"), 50);
        assert_eq!(answer["total"], 120);
        assert_eq!(answer["nextCursor"], "next");
        let rows: Vec<Value> = (0..5).map(|i| json!({"i": i})).collect();
        let answer = context_answer(5, rows, json!([]), None, |_| None);
        assert_eq!(answer["truncated"], false);
        assert_eq!(
            (&answer["returned"], &answer["total"]),
            (&json!(5), &json!(5))
        );
        assert!(
            answer.get("nextCursor").is_none(),
            "the last page has no cursor"
        );
    }

    /// SV-ENG-63 D-c: a byte-cut page points its cursor after the last row actually sent.
    #[test]
    fn a_byte_cut_page_points_its_cursor_after_the_last_row_sent() {
        let big = "x".repeat(super::super::live::MAX_ANSWER_BYTES / 4);
        let rows: Vec<Value> = (0..10).map(|_| json!({"blob": big})).collect();
        let answer = context_answer(10, rows, json!([]), Some("after-ten".into()), |kept| {
            Some(format!("after-{kept}"))
        });
        let kept = assert_marked_cut(&answer, "rows");
        assert!(kept > 0 && kept < 10);
        assert_eq!(answer["nextCursor"], format!("after-{kept}"));
    }

    /// SV-ENG-63: a usage page cut by bytes continues at the first session not sent, and the
    /// whole-window `total` and `totals` ride on every page.
    #[test]
    fn a_byte_cut_usage_page_continues_at_the_first_session_not_sent() {
        let mut report = usage_report();
        report.sessions = (0..600).map(heavy_session).collect();
        report.session_count = 900;
        report.cursor_key = 7;
        report.next_cursor = Some("after-page".into());
        let answer = usage_answer(&report, &[]);
        let kept = assert_marked_cut(&answer, "sessions");
        assert!(kept < 500);
        assert_eq!(answer["total"], 900);
        let cursor = answer["nextCursor"].as_str().unwrap();
        let last = &report.sessions[kept - 1];
        assert_eq!(
            cursor,
            format!("u:7:{}:{}", last.last_at_ms, last.session_id)
        );
        report.sessions.truncate(3);
        let answer = usage_answer(&report, &[]);
        assert_eq!(
            answer["nextCursor"], "after-page",
            "an uncut page keeps the engine's cursor"
        );
    }

    /// SV-PBF-287: the hosted queryContext pages one snapshot while the index
    /// moves, answers `StaleCursor` (wire `stale_cursor`, relay 409) for a
    /// cursor from an earlier app run, and `invalid_args` for a malformed one.
    #[tokio::test]
    async fn hosted_query_context_tells_a_stale_cursor_from_a_bad_one() {
        use crate::context_query::{index::IndexRoot, model::Layer, ContextIndexService};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("acme/alpha/content");
        let story = |id: &str| format!("---\ntype: story\nstory_id: {id}\nstatus: todo\n---\n");
        for id in ["S-1", "S-2", "S-3"] {
            let path = root.join(format!("stories/{id}.md"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, story(id)).unwrap();
        }
        std::fs::create_dir_all(root.join("dash")).unwrap();
        let svc = ContextIndexService::started(vec![IndexRoot {
            dir: root.clone(),
            org: Some("acme".into()),
            team: Some("alpha".into()),
            layer: Layer::Team,
        }]);
        let ctx = LiveCtx {
            context_index: Some(svc.clone()),
            ..Default::default()
        };
        let call = |args: Value| Call {
            args,
            fields: fields(DEFAULT_CONTEXT_FIELDS),
            folder: root.join("dash"),
            scope: Some(super::super::state::Scope {
                org: "acme".into(),
                team: "alpha".into(),
            }),
            hold: None,
            walks: None,
            walk_deadline: None,
        };
        let first = query_context(ctx.clone(), call(json!({"limit": 1})))
            .await
            .unwrap();
        let cursor = first["nextCursor"].as_str().unwrap().to_string();
        let added = root.join("stories/S-4.md");
        std::fs::write(&added, story("S-4")).unwrap();
        svc.settle(vec![added], svc.snapshot().generation);
        let next = query_context(ctx.clone(), call(json!({"limit": 1, "cursor": cursor})))
            .await
            .unwrap();
        assert_eq!(next["total"], 3, "the walk's own snapshot");
        let earlier = crate::context_query::engine::from_an_earlier_run(&cursor);
        assert_eq!(
            query_context(ctx.clone(), call(json!({"limit": 1, "cursor": earlier}))).await,
            Err(QueryError::StaleCursor)
        );
        assert!(matches!(
            query_context(ctx.clone(), call(json!({"limit": 1, "cursor": "o:1:x"}))).await,
            Err(QueryError::InvalidArgs(_))
        ));
    }

    #[test]
    fn the_hosted_args_take_a_cursor() {
        assert!(query_args("sessionUsage").contains(&"cursor") && query_args("queryContext").contains(&"cursor"));
        let usage = json!({"limit": 3, "cursor": "u:1:2:s"});
        let ours: crate::session_usage::Filter = strict(usage.clone(), query_args("sessionUsage")).unwrap();
        let mcp: crate::session_usage::Filter = serde_json::from_value(usage).unwrap();
        assert_eq!(ours.cursor.as_deref(), Some("u:1:2:s"));
        assert_eq!(format!("{ours:?}"), format!("{mcp:?}"));
        let q = context_query(
            json!({"cursor": "o:5:1:ab"}),
            &fields(DEFAULT_CONTEXT_FIELDS),
            "acme",
            "alpha",
        )
        .unwrap();
        assert_eq!(q.cursor.as_deref(), Some("o:5:1:ab"));
    }

    /// SV-ENG-61: the one helper every hosted list goes through. A limit cut and a byte cut are
    /// marked the same way, and a list that fits is left unmarked.
    #[test]
    fn bounded_rows_marks_every_cut_the_same_way() {
        let rows = |n: usize| (0..n).map(|i| json!({"i": i}));
        let put = |n: usize, matched: usize, cap: usize| {
            let mut answer = json!({"rows": []});
            bounded_rows(rows(n), matched, cap, &answer).put_in(&mut answer, "rows");
            answer
        };
        let whole = put(10, 10, 50);
        assert_eq!(whole["rows"].as_array().unwrap().len(), 10);
        assert!(["truncated", "returned", "total"]
            .iter()
            .all(|k| whole.get(*k).is_none()));
        let limit = put(120, 120, 50);
        assert_eq!(limit["rows"].as_array().unwrap().len(), 50);
        assert_eq!(
            (&limit["truncated"], &limit["returned"], &limit["total"]),
            (&json!(true), &json!(50), &json!(120))
        );
        // The source already cut to its limit: `matched` is what it had before.
        let source_cut = put(50, 300, 500);
        assert_eq!(
            (&source_cut["returned"], &source_cut["total"]),
            (&json!(50), &json!(300))
        );
        // A count below the rows given never reads as a cut.
        assert!(put(10, 3, 50).get("truncated").is_none());
        let big = "x".repeat(super::super::live::MAX_ANSWER_BYTES / 4);
        let mut answer = json!({"rows": []});
        bounded_rows((0..10).map(|_| json!({"blob": big})), 10, 500, &answer)
            .put_in(&mut answer, "rows");
        let kept = answer["rows"].as_array().unwrap().len();
        assert!(kept > 0 && kept < 10);
        assert_eq!(
            (&answer["truncated"], &answer["returned"], &answer["total"]),
            (&json!(true), &json!(kept), &json!(10))
        );
    }

    #[test]
    fn a_runs_limit_of_zero_reads_as_unset() {
        assert_eq!(runs_cap(None), DEFAULT_ROWS);
        assert_eq!(runs_cap(Some(0)), DEFAULT_ROWS);
        assert_eq!(runs_cap(Some(7)), 7);
        assert_eq!(runs_cap(Some(10_000)), MAX_ROWS);
    }

    /// An answer that fits carries no `truncated`; a row too big alone is kept so the
    /// dispatcher answers `too_large` for it.
    #[test]
    fn fit_rows_keeps_what_fits_and_always_the_first_row() {
        let small: Vec<Value> = (0..10).map(|i| json!({"i": i})).collect();
        assert_eq!(fit_rows(small.clone(), &json!([])), small);
        let mut report = usage_report();
        report.sessions = vec![heavy_session(1), heavy_session(2)];
        let fits = usage_answer(&report, &[]);
        assert_eq!(
            fits["truncated"], false,
            "D-d: every page says so, false when it is everything"
        );
        let huge = vec![
            json!({"blob": "x".repeat(super::super::live::MAX_ANSWER_BYTES)}),
            json!({"i": 2}),
        ];
        assert_eq!(fit_rows(huge, &json!([])).len(), 1);
    }

    /// SV-INP-38 review: redaction can lengthen a row (a credential-named key's strings become the
    /// placeholder), so rows are sized after it: a list that fits raw but not redacted is cut, not failed.
    #[test]
    fn rows_that_grow_under_redaction_are_fitted_after_it() {
        let cap = super::super::live::MAX_ANSWER_BYTES;
        let row = || json!({"password": "x"});
        let raw = serde_json::to_vec(&row()).unwrap().len() + 1;
        let rows: Vec<Value> = (0..cap / raw - 100).map(|_| row()).collect();
        let whole = |rows: &[Value]| {
            serde_json::to_vec(&redact(json!({"rows": rows})))
                .unwrap()
                .len()
        };
        assert!(
            serde_json::to_vec(&json!({"rows": rows})).unwrap().len() < cap,
            "fits raw"
        );
        assert!(whole(&rows) > cap, "the premise: redaction pushes it over");
        let total = rows.len();
        let mut answer = json!({"rows": []});
        bounded_rows(rows, total, MAX_ROWS, &answer).put_in(&mut answer, "rows");
        let kept = answer["rows"].as_array().unwrap().len();
        assert!(kept < total && kept > 0);
        assert_eq!(answer["returned"], kept);
        assert!(serde_json::to_vec(&redact(answer)).unwrap().len() <= cap);
    }

    /// SW-DP-18: a usage answer carries each session's story (id, title, status, epic) and
    /// its working folder only when the manifest lists them, each on its own.
    #[test]
    fn a_usage_answer_has_only_the_optional_fields_the_manifest_lists() {
        let report = usage_report();
        let answer = usage_answer(&report, &[]);
        let rows = answer["sessions"].as_array().unwrap();
        assert!(
            rows.iter()
                .all(|r| r.get("story").is_none() && r.get("cwd").is_none()),
            "{answer}"
        );
        assert_eq!(rows[0]["sessionId"], "s1");
        assert!(
            !answer.to_string().contains("/Users/me") && !answer.to_string().contains("SV-5"),
            "{answer}"
        );

        let answer = usage_answer(&report, &fields(&["story"]));
        let rows = answer["sessions"].as_array().unwrap();
        assert_eq!(
            rows[0]["story"],
            json!({"id": "SV-5", "title": "Redesign", "status": "todo", "epic": null})
        );
        assert!(rows[1]["story"].is_null() && rows[1].get("story").is_some());
        assert!(rows.iter().all(|r| r.get("cwd").is_none()), "{answer}");

        let answer = usage_answer(&report, &fields(&["cwd"]));
        assert_eq!(
            answer["sessions"][0]["cwd"],
            "/Users/me/codesense-wt/feat-SV-5"
        );
        assert!(answer["sessions"][0].get("story").is_none());
        // The machine a session ran on is never shared, whatever the manifest lists.
        for listed in [&[][..], &["story"], &["cwd"], &["story", "cwd"]] {
            let answer = usage_answer(&report, &fields(listed));
            let rows = answer["sessions"].as_array().unwrap();
            assert!(
                rows.iter().all(|r| r.get("machineId").is_none()),
                "{answer}"
            );
        }
        // ...and a manifest cannot name it.
        assert!(field_refusal(find(CATALOG, "sessionUsage").unwrap(), "machineId").is_some());
    }

    /// SW-DP-18: a viewer cannot filter by story unless the dashboard shares it, and an
    /// unknown story key is refused, both before any store is read.
    #[tokio::test]
    async fn a_story_filter_needs_the_story_field_and_an_unknown_key_is_refused() {
        let q = find(CATALOG, "sessionUsage").unwrap();
        for args in [
            json!({"story": "SV-5"}),
            json!({"storyId": "SV-5"}),
            json!({"story": 5}),
        ] {
            let got = (q.run)(LiveCtx::default(), call(args.clone())).await;
            assert!(
                matches!(got, Err(QueryError::InvalidArgs(_))),
                "{args}: {got:?}"
            );
        }
        let shared = Call {
            args: json!({"storyId": "SV-5"}),
            fields: fields(&["story"]),
            ..Default::default()
        };
        let got = (q.run)(LiveCtx::default(), shared).await;
        assert!(matches!(got, Err(QueryError::InvalidArgs(_))), "{got:?}");
    }

    /// SW-DP-18: `story` is a sessionUsage arg, parsed like the MCP tool's input.
    #[test]
    fn the_story_arg_parses_into_the_usage_filter() {
        let usage = json!({"story": "SV-5", "limit": 3});
        let ours: crate::session_usage::Filter = strict(usage.clone(), query_args("sessionUsage")).unwrap();
        let mcp: crate::session_usage::Filter = serde_json::from_value(usage).unwrap();
        assert_eq!(ours.story.as_deref(), Some("SV-5"));
        assert_eq!(format!("{ours:?}"), format!("{mcp:?}"));
    }

    /// Review S1: the dashboard fixes `queryContext`'s scope; the viewer only narrows.
    #[test]
    fn query_context_is_fixed_to_the_dashboards_team_and_fields() {
        let allowed = fields(DEFAULT_CONTEXT_FIELDS);
        let q = context_query(
            json!({"match": {"status": ["todo", "review"]}, "sort": ["-updated"], "limit": 9000, "groupBy": "status"}),
            &allowed,
            "acme",
            "alpha",
        )
        .unwrap();
        let scope = serde_json::to_value(&q.scope).unwrap();
        assert_eq!(scope["orgs"], json!(["acme"]));
        assert_eq!(scope["teams"], json!(["alpha"]));
        assert_eq!(scope["layers"], json!(["team"]));
        assert_eq!(q.text, None);
        assert_eq!(q.cursor, None);
        assert_eq!(q.limit, Some(MAX_ROWS));
        assert_eq!(q.select, Some(allowed.clone()));
        assert!(q.relationships.is_none());

        for args in [
            json!({"where": {"orgs": "all"}}),
            json!({"text": "csp_"}),
            json!({"select": ["summary"]}),
            json!({"relationships": {"parentEpic": "X"}}),
            json!({"where": {"kinds": ["story"], "orgs": "all"}}),
            json!({"where": {"kinds": ["nonsense"]}}),
            json!({"filters": [{"key": "assignee", "op": "eq", "value": "me"}]}),
            json!({"filters": [{"key": "status", "op": "prefix", "value": "t"}]}),
            json!({"filters": [{"key": "status", "op": "in", "value": "todo"}]}),
            json!({"filters": [{"key": "status", "op": "eq", "value": {"x": 1}}]}),
            json!({"select": ["status", "assignee"]}),
            json!({"match": {"assignee": "me"}}),
            json!({"match": {"status": {"op": "prefix"}}}),
            json!({"sort": ["assignee"]}),
            json!({"groupBy": "path"}),
        ] {
            assert!(
                matches!(
                    context_query(args.clone(), &allowed, "acme", "alpha"),
                    Err(QueryError::InvalidArgs(_))
                ),
                "{args} was accepted"
            );
        }
        // No limit: the cap, never more.
        let q = context_query(json!({}), &allowed, "acme", "alpha").unwrap();
        assert_eq!(q.limit, Some(MAX_ROWS));
    }

    /// SV-INP-29: the local SDK's `where.kinds`, eq/in `filters` and `select` are
    /// accepted as narrowing only: the team and layer stay the dashboard's.
    #[test]
    fn query_context_accepts_the_local_subset_as_narrowing() {
        let allowed = fields(&["status", "title", "sprint", "priority", "type"]);
        let q = context_query(
            json!({
                "where": {"kinds": ["story", "bug", "spike"]},
                "filters": [{"key": "sprint", "op": "eq", "value": "1"}, {"key": "status", "op": "in", "value": ["todo", "review"]}],
                "select": ["status", "priority"],
                "sort": ["-mtime"],
                "limit": 400,
            }),
            &allowed,
            "acme",
            "alpha",
        )
        .unwrap();
        let scope = serde_json::to_value(&q.scope).unwrap();
        assert_eq!(scope["orgs"], json!(["acme"]));
        assert_eq!(scope["teams"], json!(["alpha"]));
        assert_eq!(scope["layers"], json!(["team"]));
        assert_eq!(scope["kinds"], json!(["story", "bug", "spike"]));
        assert_eq!(q.filters.len(), 2);
        assert_eq!(
            q.select,
            Some(vec!["status".to_string(), "priority".to_string()])
        );
        // A filter and a match on one key would silently AND; it is refused.
        assert!(context_query(
            json!({"match": {"sprint": "1"}, "filters": [{"key": "sprint", "op": "eq", "value": "2"}]}),
            &allowed,
            "acme",
            "alpha"
        )
        .is_err());
    }

    #[test]
    fn a_context_row_carries_no_path_summary_or_unlisted_field() {
        let row: crate::context_query::model::ContextRow = serde_json::from_value(json!({
            "id": "SW-1", "path": "/Users/me/.codesense/orgs/acme/repo/alpha/content/SW-1.md",
            "relPath": "content/SW-1.md", "fileName": "SW-1.md", "org": "acme", "team": "alpha",
            "layer": "team", "kind": "story", "mtime": 1, "title": "Login",
            "fields": {"status": "todo", "assignee": "@me", "notes": "csp_secret"},
            "summary": "the body text"
        }))
        .unwrap();
        let out = context_row(&row, &fields(&["status", "title"])).to_string();
        assert!(!out.contains("/Users/me"), "{out}");
        assert!(
            !out.contains("body text") && !out.contains("assignee") && !out.contains("csp_"),
            "{out}"
        );
        assert!(
            out.contains("\"status\":\"todo\"") && out.contains("Login"),
            "{out}"
        );
    }

    #[test]
    fn the_team_comes_from_where_the_dashboard_lives() {
        use crate::context_query::{index::IndexRoot, model::Layer};
        let roots = vec![
            IndexRoot {
                dir: "/c/orgs/acme/repo/alpha".into(),
                org: Some("acme".into()),
                team: Some("alpha".into()),
                layer: Layer::Team,
            },
            IndexRoot {
                dir: "/c/private/acme".into(),
                org: Some("acme".into()),
                team: None,
                layer: Layer::Private,
            },
        ];
        assert_eq!(
            team_of(
                &roots,
                std::path::Path::new("/c/orgs/acme/repo/alpha/content/dash")
            ),
            Some(("acme".into(), "alpha".into()))
        );
        assert_eq!(
            team_of(&roots, std::path::Path::new("/c/private/acme/dash")),
            None
        );
        assert_eq!(team_of(&roots, std::path::Path::new("/tmp/dash")), None);
    }

    #[test]
    fn manifest_fields_are_checked_per_query() {
        let ctx = find(CATALOG, "queryContext").unwrap();
        assert_eq!(field_refusal(ctx, "priority"), None);
        assert!(field_refusal(ctx, "summary").is_some());
        assert!(field_refusal(ctx, "a b").is_some());
        let runs = find(CATALOG, "runs").unwrap();
        assert_eq!(field_refusal(runs, "summary"), None);
        assert!(field_refusal(runs, "artifact_ref").is_some());
        assert_eq!(effective_fields(ctx, None), fields(DEFAULT_CONTEXT_FIELDS));
        assert!(effective_fields(runs, None).is_empty());
    }

    /// Review S1: free text is dropped unless the manifest names it.
    #[test]
    fn optional_fields_are_left_out_unless_named() {
        let row = json!({"handle": "h", "workspace": "/Users/me/w", "unread": 2});
        assert_eq!(
            pick(&row, SESSION_KEYS, &[]),
            json!({"handle": "h", "unread": 2})
        );
        assert_eq!(
            pick(&row, SESSION_KEYS, &fields(&["workspace"])),
            json!({"handle": "h", "workspace": "/Users/me/w", "unread": 2})
        );
    }

    /// SW-DP-5 AC-3 (DP-INV-10): a publisher token, an Anthropic key and a
    /// GitHub token are redacted wherever they sit in an answer.
    #[test]
    fn answers_are_redacted() {
        let answer = json!({
            "sessions": [{
                "cwd": "/Users/me/work",
                "note": "token csp_live_abcdef0123456789 in use",
                "env": "ANTHROPIC_API_KEY sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123",
                "gh": ["ghp_abcdefghijklmnopqrstuvwxyz0123"],
                "csb_session_secretkey0123": 1,
                "viewer": "cvs_0123456789abcdef"
            }],
            "count": 3
        });
        let out = redact(answer).to_string();
        for leaked in [
            "csp_live_abcdef0123456789",
            "sk-ant-api03",
            "ghp_abcdefghijklmnopqrstuvwxyz0123",
            "csb_session",
            "cvs_0123456789abcdef",
        ] {
            assert!(!out.contains(leaked), "{leaked} survived: {out}");
        }
        assert!(
            out.contains("/Users/me/work"),
            "a plain value was changed: {out}"
        );
        assert!(out.contains(r#""count":3"#));
        assert!(out.contains(REDACTION_PLACEHOLDER));
    }

    /// Review S2: a value under a credential-named key is replaced, at any depth.
    #[test]
    fn values_under_credential_keys_are_redacted() {
        let answer = json!({
            "config": {
                "password": "hunter2hunter2",
                "db": {"Passwd": "x1", "apiKey": "plain-looking", "API_KEY": ["v1", "v2"]},
                "client_secret": "abc",
                "authorization": "Basic Zm9v",
                "auth": {"user": "u", "pass": "p"},
                "privateKey": "-----x",
                "credentials": [{"value": "c"}]
            },
            "tokens": {"input": 10, "output": 20},
            "author": "Ada",
            "tokenizer": "cl100k"
        });
        let out = redact(answer);
        let text = out.to_string();
        for leaked in [
            "hunter2",
            "x1",
            "plain-looking",
            "v1",
            "\"abc\"",
            "Zm9v",
            "\"u\"",
            "-----x",
            "\"c\"",
        ] {
            assert!(!text.contains(leaked), "{leaked} survived: {text}");
        }
        assert_eq!(
            out["tokens"],
            json!({"input": 10, "output": 20}),
            "token counts were wiped"
        );
        assert_eq!(out["author"], "Ada");
        assert_eq!(out["tokenizer"], "cl100k");
        assert!(
            sensitive_key("x-api-key") && sensitive_key("ApiKey") && sensitive_key("access_token")
        );
        assert!(!sensitive_key("authorName") && !sensitive_key("keystone"));
    }

    /// Re-review: the contract states which org and team queryContext reads,
    /// and keeps no placeholder.
    #[test]
    fn query_context_names_its_org_and_team_rule() {
        let q = find(CATALOG, "queryContext").unwrap();
        assert!(q.shares.contains(SCOPE_MARK));
        let rule = shares_rule(q);
        assert!(
            rule.starts_with("per file in the one org and team"),
            "{rule}"
        );
        let contract = crate::mcp::live_data_authoring::hosted_section(CATALOG).to_string();
        assert!(contract.contains("whose context repo folder holds the dashboard folder"));
        for text in [&rule, &contract] {
            assert!(
                !text.contains(SCOPE_MARK) && !text.contains("own org and team"),
                "{text}"
            );
            assert!(!text.contains("dialog"), "{text}");
        }
    }

    /// Security review: queryContext reads only the team saved at publish.
    /// Case 1: the scope was unknown at publish (index not running): refused.
    /// Case 2: a deeper team repo attached later moves the folder to another
    /// team: refused. Only the saved team, still resolved, is read.
    #[test]
    fn query_context_reads_only_the_saved_team() {
        use super::super::state::Scope;
        let emea = Scope {
            org: "acme".into(),
            team: "emea".into(),
        };
        let now = |o: &str, t: &str| Some((o.to_string(), t.to_string()));

        // Case 1: saved with no scope known.
        assert!(matches!(
            check_scope(None, now("acme", "emea")),
            Err(QueryError::NotApproved(_))
        ));
        assert!(matches!(
            check_scope(None, None),
            Err(QueryError::NotApproved(_))
        ));

        // Case 2: the folder now resolves to a deeper team repo.
        let root = |dir: &str, team: &str| crate::context_query::index::IndexRoot {
            dir: dir.into(),
            org: Some("acme".into()),
            team: Some(team.into()),
            layer: crate::context_query::model::Layer::Team,
        };
        let roots = vec![
            root("/c/acme/emea", "emea"),
            root("/c/acme/emea/content/sub", "emea-sub"),
        ];
        let resolved = team_of(
            &roots,
            std::path::Path::new("/c/acme/emea/content/sub/dash"),
        );
        assert_eq!(resolved, now("acme", "emea-sub"));
        assert!(matches!(
            check_scope(Some(&emea), resolved),
            Err(QueryError::NotApproved(_))
        ));
        // Or to no team at all.
        assert!(matches!(
            check_scope(Some(&emea), None),
            Err(QueryError::NotApproved(_))
        ));

        // The saved team, still resolved: read.
        assert_eq!(
            check_scope(Some(&emea), now("acme", "emea")).unwrap(),
            ("acme".into(), "emea".into())
        );
    }

    /// Re-review: acronym joins and the further credential names are caught;
    /// ordinary keys that merely contain those letters are not.
    #[test]
    fn credential_key_names_from_the_re_review_are_sensitive() {
        assert_eq!(key_words("APIToken"), ["api", "token"]);
        assert_eq!(key_words("JWTSecret"), ["jwt", "secret"]);
        assert_eq!(
            key_words("aws_access_key_id"),
            ["aws", "access", "key", "id"]
        );
        assert_eq!(key_words("DATABASE_URL"), ["database", "url"]);
        assert_eq!(key_words("userID2Name"), ["user", "id2", "name"]);
        for key in [
            "APIToken",
            "JWTSecret",
            "aws_access_key_id",
            "accessKey",
            "signing_key",
            "encryption_key",
            "session_key",
            "secretKey",
            "private_keys",
            "pwd",
            "DB_PWD",
            "passphrase",
            "sshPassphrase",
            "cookie",
            "Set-Cookie",
            "bearer",
            "bearerValue",
            "dsn",
            "SENTRY_DSN",
            "DATABASE_URL",
            "databaseUrl",
            "databaseurl",
        ] {
            assert!(sensitive_key(key), "{key} was not treated as a credential");
        }
        for key in [
            "key",
            "sortKey",
            "keyboard",
            "session_id",
            "sessionCount",
            "access_count",
            "signingDate",
            "database",
            "url",
            "cookbook",
            "passage",
            "author",
            "tokenizer",
        ] {
            assert!(!sensitive_key(key), "{key} was treated as a credential");
        }
        let out = redact(json!({
            "APIToken": "t1", "JWTSecret": "t2", "aws_access_key_id": "t3",
            "accessKey": "t4", "signing_key": "t5", "encryption_key": "t6",
            "session_key": "t7", "pwd": "t8", "passphrase": "t9", "cookie": "t10",
            "bearer": "t11", "dsn": "t12", "DATABASE_URL": "t13",
            // A token count under a credential-looking key keeps its number.
            "sortKey": "title", "inputTokensUsed": 42
        }));
        for n in 1..=13 {
            assert_eq!(
                out[[
                    "APIToken",
                    "JWTSecret",
                    "aws_access_key_id",
                    "accessKey",
                    "signing_key",
                    "encryption_key",
                    "session_key",
                    "pwd",
                    "passphrase",
                    "cookie",
                    "bearer",
                    "dsn",
                    "DATABASE_URL",
                ][n - 1]],
                REDACTION_PLACEHOLDER,
                "t{n} survived"
            );
        }
        assert_eq!(out["sortKey"], "title");
        assert_eq!(out["inputTokensUsed"], 42);
    }

    /// SW-DP-5 AC-3: the catalog names no credential store, Keychain or token module.
    #[test]
    fn the_catalog_source_names_no_credential_module() {
        let source = include_str!("catalog.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        for needle in [
            "credential_store",
            "keyring",
            "Secret",
            "TOKEN_SERVICE",
            "TOKEN_ACCOUNT",
        ] {
            assert!(!code.contains(needle), "catalog.rs names {needle}");
        }
    }
}

#[cfg(test)]
mod caller_usage_tests {
    use super::*;
    #[test]
    fn published_usage_enrichment_uses_dashboard_source_authority() {
        use crate::context_repo::caller_scope::tests::AppFixture;
        use crate::session_usage as usage;
        let f = AppFixture::new();
        let hidden = f.fixture.write(
            "parent",
            "content/published-usage.md",
            "---\ntype: story\ntitle: Hidden published title\nstatus: in-progress\n---\n",
        );
        let mut store = usage::UsageStore::open(&f.fixture.home.join("usage.sqlite")).unwrap();
        store
            .ingest(&[usage::tests::event(
                "published",
                "published-session",
                "claude-opus-4-8",
                usage::tests::local_noon(2026, 10, 15),
                usage::event::Tokens {
                    input: 1,
                    ..Default::default()
                },
            )])
            .unwrap();
        store
            .link_story(
                "published-session",
                &usage::StoryLink {
                    story_id: "P-USAGE".into(),
                    source: hidden.clone(),
                    org: "acme/ctx".into(),
                },
            )
            .unwrap();
        let read = || {
            tauri::async_runtime::block_on(session_usage(
                LiveCtx::default(),
                Call {
                    args: serde_json::json!({}),
                    fields: vec!["story".into()],
                    folder: f.fixture.path("child", "content"),
                    ..Default::default()
                },
            ))
            .unwrap()
        };
        let denied = read();
        assert_eq!(denied["sessions"][0]["story"]["title"], Value::Null);
        std::fs::remove_file(hidden).unwrap();
        assert_eq!(denied, read());
        assert!(f
            .fixture
            .scope
            .existing(&f.fixture.path("parent", "content/shared.md"))
            .is_ok());
    }
}

#[cfg(test)]
mod why_line_tests {
    use super::*;
    use crate::attribution::git_ai::tests::{rebuild, CLAUDE};
    use crate::attribution::ranges::AttributionSource;
    use crate::context_repo::caller_scope::tests::AppFixture;
    use crate::session_usage::{
        record::{RecordAccount, SessionRecord},
        UsageStore,
    };
    use tauri::Manager;

    #[test]
    fn published_turn_and_unavailable_use_nested_allow_lists() {
        let raw=json!({"path":"a.txt","ranges":[{"start":1,"end":2,"turn":{"sessionId":"s","toolUseId":"toolu_x","turn":0,"basis":"commit","taskId":"task_x","taskBasis":"interval","reason":"conversation expired","conversation":"private text","path":"/private/file"},"unavailable":{"turn":"edit not recorded by git-ai","debug":"private debug"}}],"sessions":{}});
        let projected=project_why_line(&raw);
        assert_eq!(projected["ranges"][0]["turn"],json!({"sessionId":"s","toolUseId":"toolu_x","turn":0,"basis":"commit","taskId":"task_x","taskBasis":"interval","reason":"conversation expired"}));
        assert_eq!(projected["ranges"][0]["unavailable"],json!({"turn":"edit not recorded by git-ai"}));
        assert!(!projected.to_string().contains("private"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn catalog_timeout_retains_slot_until_blocking_git_read_exits() {
        use std::time::Duration;
        let f = AppFixture::new();
        let repo = rebuild();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::new(slots.clone().acquire_owned().await.unwrap());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let call = Call {
            args: json!({"repo":"acme/api","path":"a.txt","start":1,"end":2}),
            hold: Some(permit),
            ..Default::default()
        };
        let query = why_line_for_with(
            Some(f.app.handle().clone()),
            call,
            move |_app, _params, _folder, hold| async move {
                crate::attribution::why_line::read_held(hold, move || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    let result = crate::attribution::git_ai::GitAiSource::default().ranges(
                        repo.path(),
                        "HEAD",
                        "a.txt",
                    );
                    let _ = finished_tx.send(());
                    result.map(|_| json!({})).map_err(|error| error.to_string())
                })
                .await
            },
        );
        let timed_out = tokio::time::timeout(Duration::from_millis(20), query)
            .await
            .is_err();
        entered_rx.await.unwrap();
        let early = slots.clone().try_acquire_owned();
        let remained_held = early.is_err();
        drop(early);
        release_tx.send(()).unwrap();
        finished_rx.await.unwrap();
        let released =
            tokio::time::timeout(Duration::from_secs(2), slots.clone().acquire_owned()).await;
        assert!(timed_out);
        assert!(
            remained_held,
            "timed-out blocking blame must retain the published slot"
        );
        assert!(
            released.is_ok(),
            "slot returns when blocking Git work exits"
        );
    }

    #[tokio::test]
    async fn catalog_why_line_is_registered_and_rejects_bad_args_before_reading() {
        let entry = find(CATALOG, "whyLine").expect("published whyLine entry");
        assert!(matches!(entry.fields, Fields::Named([])));
        assert!(std::ptr::fn_addr_eq(entry.run, why_line as RunFn));
        for args in [
            json!({}),
            json!({"repo":"acme/api","path":"a.txt","start":"1","end":2}),
            json!({"repo":"acme/api","path":"a.txt","start":1,"end":2,"extra":true}),
        ] {
            assert!(matches!(
                (entry.run)(
                    Default::default(),
                    Call {
                        args,
                        ..Default::default()
                    }
                )
                .await,
                Err(QueryError::InvalidArgs(_))
            ));
        }
    }

    #[tokio::test]
    async fn catalog_why_line_reads_real_git_and_live_session_with_only_shared_fields() {
        let f = AppFixture::new();
        let repo = rebuild();
        crate::attribution::why_line::tests::attach(&f.fixture.home, repo.path());
        // An omitted rev reads origin's default branch, never the checkout's HEAD.
        crate::attribution::git_ai::tests::git(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let store = UsageStore::open(&f.fixture.home.join("usage.sqlite")).unwrap();
        let mut record = SessionRecord::new("published-session");
        record.native_session_id = Some(CLAUDE.into());
        record.cli = Some("claude".into());
        record.worktree = Some("/private/published-worktree".into());
        record.account = Some(RecordAccount {
            provider: Some("fixture".into()),
            account_id: Some("private-account".into()),
            label: Some("owner@example.com".into()),
            source: Some("fixture".into()),
        });
        store.write_session_record(&record).unwrap();
        drop(store);
        crate::attribution::turns::tests::checkpoint_log(repo.path(), "a.txt", &["toolu_01CCC"]);
        crate::attribution::turns::tests::capture(
            &f.fixture.home,
            &f.fixture.home.join("session-conversations"),
            "published-session",
            None,
            &crate::session_conversation::Policy::default(),
        );
        let call = || Call {
            args: json!({"repo":"acme/api","path":"a.txt","start":1,"end":3}),
            folder: f.fixture.path("child", "content"),
            ..Default::default()
        };
        let answer = why_line_for(Some(f.app.handle().clone()), call())
            .await
            .unwrap();
        assert_eq!(answer["ranges"][0]["link"], "linked");
        assert_eq!(answer["ranges"][0]["sessionId"], "published-session");
        let turn = &answer["ranges"][0]["turn"];
        assert_eq!(
            (turn["sessionId"].as_str(), turn["toolUseId"].as_str(), turn["turn"].as_u64(), turn["basis"].as_str()),
            (Some("published-session"), Some("toolu_01CCC"), Some(2), Some("edit"))
        );
        assert!(answer["ranges"][0].get("traceId").is_none());
        assert_eq!(
            answer["sessions"]["published-session"]["sessionId"],
            "published-session"
        );
        let params = serde_json::from_value(call().args).unwrap();
        let mut raw =
            crate::commands::data_read_model::why_line(f.app.handle().clone(), params, None, None)
                .await
                .unwrap();
        assert_eq!(
            raw["sessions"]["published-session"]["session"]["account"]["accountId"],
            "private-account"
        );
        assert_eq!(
            raw["sessions"]["published-session"]["session"]["worktree"],
            "/private/published-worktree"
        );
        raw["local_path"] = json!("/private/source");
        raw["subject"] = json!("private commit subject");
        raw["conversation"] = json!(["private conversation"]);
        raw["ranges"][0]["subject"] = json!("private commit subject");
        raw["sessions"]["published-session"]["conversation"] = json!(["private conversation"]);
        raw["sessions"]["published-session"]["session"]["timing"] = json!({"startedAtMs":100,"endedAtMs":300,"wallMs":200,"activeMs":100,"idleMs":100,"turnCount":1,"toolCallCount":0,"timingSource":"fixture","idleGaps":[{"note":"private idle note"}],"futureField":"private timing"});
        let projected = project_why_line(&raw);
        for key in ["local_path", "subject", "conversation"] {
            assert!(projected.get(key).is_none(), "top field {key}");
        }
        for key in [
            "nativeSessionId",
            "subject",
            "record",
            "account",
            "conversation",
        ] {
            assert!(
                projected["ranges"][0].get(key).is_none(),
                "range field {key}"
            );
        }
        let session = &projected["sessions"]["published-session"];
        for key in [
            "session",
            "nativeSessionIds",
            "record",
            "account",
            "conversation",
            "worktree",
            "story",
            "tasks",
            "run",
            "tools",
            "unavailable",
        ] {
            assert!(session.get(key).is_none(), "session field {key}");
        }
        assert_eq!(
            session["timing"],
            json!({"startedAtMs":100,"endedAtMs":300,"wallMs":200,"activeMs":100,"idleMs":100,"turnCount":1,"toolCallCount":0,"timingSource":"fixture"})
        );
        let text = projected.to_string();
        for private in [
            "/private/",
            "private-account",
            "owner@example.com",
            "private conversation",
            "private commit subject",
            "private idle note",
            "private timing",
            "idleGaps",
            "futureField",
        ] {
            assert!(!text.contains(private), "private value {private}");
        }
        assert_eq!(
            project_why_line(
                &json!({"status":"unavailable","reason":"no local copy","local_path":"/private/repo"})
            ),
            json!({"status":"unavailable","reason":"no local copy"})
        );
    }
}
