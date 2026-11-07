// SPDX-License-Identifier: MIT
//! SW-DP-21 (P17): the live agent streams a dashboard may declare.
//!
//! - `terminals` (value): the sessions running on this Mac in the dashboard's
//!   own team, with a live terminal, as the `sessions` query shows them plus
//!   `workspace` and `watchable`. Re-read every [`LIST_EVERY`] while watched and
//!   sent only when the redacted value changed.
//! - `workspaces` (value): this Mac's open workspaces in that team, from the
//!   live layout, with their watchable sessions and tab count. Same re-read.
//! - `terminal` (bytes, args `{handle}`): one of those sessions' terminal, as
//!   the app's own panes get it: the host's snapshot (cut to K30), then the
//!   ordered output of the app's stream (`StreamIds`) under the `dashboard`
//!   viewport. The snapshot (before its cut) and the output are redacted as one
//!   lexical stream (`TerminalRedactor`), except the start of a secret sent
//!   before it could be recognised and a value broken up by a control
//!   character. Watching never types and never resizes;
//!   only the owner's control (SW-DP-23, `live.rs`) types, through
//!   [`AgentHost::write`] and [`AgentHost::resize`].
//!
//! The team is the org and team whose context repo holds the dashboard folder
//! (`catalog::team_of`, as `queryContext`), and must be the one saved at
//! publish when one was saved (`catalog::check_scope`). Only sessions on this
//! Mac are listed or opened (DP-D49). An unknown handle and a handle of another
//! team get the same `not_found`, so a viewer cannot probe other teams.
//!
//! Everything the streams read of the app goes through [`AgentHost`], so the
//! tests drive them with a fixture and the app with [`AppAgents`].

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use super::catalog::{self, Items, LiveCtx, QueryError, StreamCall, StreamItem};
use crate::mcp::session_list::{SessionListRow, SessionState};
use crate::shared::secret_scan;

/// How often a watched list stream reads again (AC-2: a change is sent within 3 s).
pub(crate) const LIST_EVERY: Duration = Duration::from_millis(1500);
/// How often a watched terminal checks it is still in the dashboard's team.
pub(crate) const TEAM_CHECK_EVERY: Duration = Duration::from_secs(5);
/// How long a watched stream waits out a context index that serves no team (a profile switch or
/// an attach or detach empties it until the walk finishes) before it ends `failed`.
pub(crate) const INDEXING_PATIENCE: Duration = Duration::from_secs(60);
/// The `Failed` detail of a team check made while the context index is not ready.
pub(crate) const INDEXING: &str = "indexing";
/// K30: a terminal snapshot holds at most the most recent 2,000 lines…
pub(crate) const SNAPSHOT_LINES: usize = 2000;
/// …and at most 1 MB as JSON (the link's ceiling for any snapshot), less room
/// for the reset that starts a cut snapshot.
pub(crate) const SNAPSHOT_JSON_BYTES: usize = super::live::MAX_SNAPSHOT_BYTES - 64;
/// The viewport a dashboard watches under. It never sizes the terminal.
pub(crate) const VIEWPORT: &str = "dashboard";
/// Worktree branches are read at most this often (each read runs git per worktree).
pub(crate) const WORKTREES_EVERY: Duration = Duration::from_secs(30);
/// The longest handle a `terminal` open takes (handles are short opaque ids).
pub(crate) const MAX_HANDLE: usize = 128;
/// Resets the terminal's attributes at the start of a cut snapshot.
const RESET: &str = "\x1b[0m";

/// An org and team.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Team {
    pub org: String,
    pub team: String,
}

/// One tab of this Mac's live layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LayoutTab {
    /// `{org}:{team}|{workspace}`; never sent.
    pub workspace_key: String,
    /// The workspace id within its team.
    pub workspace: String,
}

/// A story worktree's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeBranch {
    pub story: String,
    pub org: Option<String>,
    pub team: Option<String>,
    pub branch: Option<String>,
}

/// What a fresh subscribe gives: the host's terminal, `vt` reproducing it at `seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalSnap {
    pub cols: u16,
    pub rows: u16,
    pub seq: u64,
    pub vt: String,
}

/// What follows a snapshot on the app's stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TermEvent {
    Output { data: String, seq: u64 },
    Resized { cols: u16, rows: u16 },
    Exited,
}

/// One subscribe of the `dashboard` viewport, undone by [`AgentHost::unsubscribe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WatchHandle {
    /// The app's stream id (`reserve_terminal_stream`).
    pub stream: u64,
    /// The app event listeners feeding `events`.
    pub listeners: Vec<tauri::EventId>,
}

pub(crate) struct TerminalWatch {
    pub snapshot: TerminalSnap,
    pub handle: WatchHandle,
    pub events: tokio::sync::mpsc::UnboundedReceiver<TermEvent>,
}

/// What the agent streams read of this Mac.
pub(crate) trait AgentHost: Send + Sync + 'static {
    /// The team whose context repo holds `folder` (`catalog::team_of`), with whether the index it
    /// was read from had finished building. Reads the disk.
    fn folder_team(&self, folder: &Path) -> TeamLookup;
    /// Every session as `list_sessions` (and the `sessions` query) reads it.
    fn sessions(&self) -> BoxFuture<'static, Result<Vec<SessionListRow>, String>>;
    /// The team `handle` was launched in (`DashboardLaunchState::launch_team`).
    fn launch_team(&self, handle: &str) -> Option<Team>;
    /// The session `handle` is bound to (`DashboardLaunchState::resolve`).
    fn resolve(&self, handle: &str) -> Option<String>;
    /// This Mac's live layout (`LiveTabs::read_local`).
    /// `None` when it could not be read (the pass is skipped).
    fn layout(&self) -> BoxFuture<'static, Option<Vec<LayoutTab>>>;
    /// The story worktrees' branches (`list_worktrees`).
    /// `None` when they could not be read (the last ones are kept).
    fn worktrees(&self) -> BoxFuture<'static, Option<Vec<WorktreeBranch>>>;
    /// The title of the story `id` in `team`'s context, when `id` is one.
    fn story_title(&self, team: &Team, id: &str) -> Option<String>;
    /// Subscribe the `dashboard` viewport to `session`'s terminal.
    fn subscribe(&self, session: String) -> BoxFuture<'static, Result<TerminalWatch, String>>;
    /// Undo a subscribe. Never blocks the caller.
    fn unsubscribe(&self, handle: WatchHandle);
    /// SW-DP-23: type `data` into `session`, as the owner typed it on a
    /// dashboard. The app's own write path (`commands::pty::write_session_input`:
    /// `write_pty` → `pty_backend::with_session_reference` →
    /// `TerminalDomain::write`, then `note_human_input`). Never logs `data`.
    fn write(&self, session: String, data: String) -> BoxFuture<'static, Result<(), String>>;
    /// SW-DP-23: ask for `cols` x `rows` as the `dashboard` viewer
    /// (`commands::pty::resize_session_as` → `TerminalDomain::resize`).
    fn resize(
        &self,
        session: String,
        cols: u16,
        rows: u16,
    ) -> BoxFuture<'static, Result<(), String>>;
    /// SV-INP-30: the face ref (`pack:id`, a colour or a glyph) the saved Agent `key`
    /// wears by default. Reads the disk. Default: none.
    fn agent_avatar(&self, _key: &str) -> Option<String> {
        None
    }
    /// SV-INP-30: the signed-in human's own face ref (a pack face or a `data:` image).
    fn human_avatar(&self) -> Option<String> {
        None
    }
    /// SV-INP-30: the refs of the faces `team` deals to an agent that has none, in order,
    /// when the team set a default pack with faces; `None` deals from the built-in funny one.
    /// Reads the disk.
    fn default_pool(&self, _team: &Team) -> Option<Vec<String>> {
        None
    }
    /// SV-INP-30: the source of the pack face `pack:id` as `team` sees it (its packs,
    /// then the user's own), with its format. Reads the disk. Shipped packs are not
    /// looked up here.
    fn avatar_face(
        &self,
        _team: &Team,
        _pack: &str,
        _id: &str,
    ) -> Option<(crate::avatars::FaceFormat, Vec<u8>)> {
        None
    }
}

pub(crate) fn host_of(ctx: &LiveCtx) -> Result<Arc<dyn AgentHost>, QueryError> {
    if let Some(host) = &ctx.agents {
        return Ok(host.clone());
    }
    let app = ctx
        .app
        .clone()
        .ok_or_else(|| QueryError::Failed("the app is not running".into()))?;
    Ok(Arc::new(AppAgents { app }))
}

/// One team lookup: the team, and whether the index snapshot it came from was ready. Both come
/// from the same snapshot, so an empty one that a reset published is never read as "no team".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeamLookup {
    pub team: Option<Team>,
    pub ready: bool,
}

/// The dashboard's team now: `no_team` when its folder is in no team's context
/// repo or no team was saved at publish (the same `check_scope` rule as
/// `queryContext`: publishing again while CodeSense knows the team fixes it),
/// `not_found` when the folder's team is not the one saved (a team change). While the context
/// index is not ready, no team is found for any folder: that is `Failed("indexing")`, which a
/// query answers `failed` and a watched stream waits out, never `no_team`.
pub(crate) async fn scoped_team(
    host: &Arc<dyn AgentHost>,
    folder: &Path,
    scope: Option<&super::state::Scope>,
) -> Result<Team, QueryError> {
    let TeamLookup { team: now, ready } = folder_team(host, folder).await;
    let Some(now) = now else {
        // Judged by the snapshot the lookup read: an empty one is not a folder in no team.
        if !ready {
            return Err(QueryError::Failed(INDEXING.into()));
        }
        return Err(QueryError::Refused("no_team"));
    };
    if scope.is_none() {
        return Err(QueryError::Refused("no_team"));
    }
    catalog::check_scope(scope, Some((now.org, now.team)))
        .map(|(org, team)| Team { org, team })
        .map_err(|_| QueryError::Refused("not_found"))
}

/// The `end` reason when the team check fails mid-stream: the refusal's own
/// (`no_team` for a folder that left every team), else `not_found`.
fn end_reason(error: Option<QueryError>) -> &'static str {
    match error {
        Some(QueryError::Refused(code)) => code,
        _ => "not_found",
    }
}

/// What one team check of a watched stream decides.
enum TeamVerdict {
    Holds,
    Retry,
    End(&'static str),
}

/// The team checks of one watched stream: two misses in a row end it (one is an index being
/// updated), but an index that is not ready is waited out for [`INDEXING_PATIENCE`] and then
/// ends `failed`, which the page opens again.
#[derive(Default)]
struct TeamWatch {
    misses: u8,
    indexing_since: Option<Instant>,
}

impl TeamWatch {
    fn judge(&mut self, found: Result<Team, QueryError>, team: &Team) -> TeamVerdict {
        match found {
            Ok(now) if &now == team => {
                *self = Self::default();
                TeamVerdict::Holds
            }
            Err(QueryError::Failed(why)) if why == INDEXING => {
                self.misses = 0;
                let since = *self.indexing_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= INDEXING_PATIENCE {
                    TeamVerdict::End("failed")
                } else {
                    TeamVerdict::Retry
                }
            }
            other => {
                self.indexing_since = None;
                self.misses += 1;
                if self.misses >= 2 {
                    TeamVerdict::End(end_reason(other.err()))
                } else {
                    TeamVerdict::Retry
                }
            }
        }
    }
}

/// `folder_team` off the async threads (it canonicalizes paths).
async fn folder_team(host: &Arc<dyn AgentHost>, folder: &Path) -> TeamLookup {
    let host = host.clone();
    let folder = folder.to_path_buf();
    tokio::task::spawn_blocking(move || host.folder_team(&folder))
        .await
        .unwrap_or(TeamLookup { team: None, ready: false })
}

/// Whether `row` is a session a dashboard of `team` lists and may open (AC-1):
/// a live terminal tab on this Mac, bound to a session, launched in `team`.
pub(crate) fn watchable(row: &SessionListRow, team: &Team, host: &dyn AgentHost) -> bool {
    // A tab on this Mac: a machine (its tab was found in a live layout) and no remote machine id.
    row.machine_id.is_none()
        && row.machine.is_some()
        && !matches!(row.state, SessionState::Exited | SessionState::Offline)
        && host.launch_team(&row.handle).as_ref() == Some(team)
        && host.resolve(&row.handle).is_some()
}

/// Workspace names, stories and branches for one pass.
pub(crate) struct Names<'a> {
    host: &'a dyn AgentHost,
    team: &'a Team,
    branches: &'a [WorktreeBranch],
    titles: RefCell<HashMap<String, Option<String>>>,
}

impl<'a> Names<'a> {
    pub fn new(host: &'a dyn AgentHost, team: &'a Team, branches: &'a [WorktreeBranch]) -> Self {
        Names {
            host,
            team,
            branches,
            titles: RefCell::new(HashMap::new()),
        }
    }

    fn title(&self, id: &str) -> Option<String> {
        self.titles
            .borrow_mut()
            .entry(id.to_string())
            .or_insert_with(|| self.host.story_title(self.team, id))
            .clone()
    }

    /// `{id, name, story?, branch?}` (AC-9): `name` is the story's title when
    /// `id` is a story in the team, else [`friendly_workspace_name`]; `id` is
    /// [`public_workspace_id`] (no local path); `story` is the row's story (or
    /// `id` when it is one); `branch` is that story's worktree branch.
    pub fn workspace(&self, id: &str, story: Option<&str>) -> Value {
        let title = self.title(id);
        let story = story
            .map(str::to_string)
            .or_else(|| title.as_ref().map(|_| id.to_string()));
        let name = title.unwrap_or_else(|| friendly_workspace_name(id));
        let mut out = json!({"id": public_workspace_id(id), "name": name});
        if let Some(story) = story {
            let same =
                |mine: &Option<String>, want: &str| mine.as_deref().is_none_or(|m| m == want);
            let branch = self
                .branches
                .iter()
                .find(|b| {
                    b.story == story
                        && same(&b.org, &self.team.org)
                        && same(&b.team, &self.team.team)
                })
                .and_then(|b| b.branch.clone());
            if let Some(branch) = branch {
                out["branch"] = json!(branch);
            }
            out["story"] = json!(story);
        }
        out
    }
}

/// The viewer workspace forms whose rest is a folder (or a repo slug).
const PATH_FORMS: &[&str] = &["generic:", "root:", "story:"];

/// True when a workspace id carries a path: viewers never get it as it is.
fn has_path(id: &str) -> bool {
    id.contains('/') || id.contains('\\')
}

/// A workspace's name for viewers when it is no titled story: the last folder
/// of a `generic:`/`root:`/`story:` id (`generic:/Users/me/src/app` is `app`),
/// else the id itself (`ops`, `SW-3`). Never a whole path.
pub(crate) fn friendly_workspace_name(id: &str) -> String {
    let rest = PATH_FORMS
        .iter()
        .find_map(|form| id.strip_prefix(form))
        .unwrap_or(id);
    let base = rest
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    if base.is_empty() || (base.ends_with(':') && has_path(rest)) {
        "Workspace".to_string()
    } else {
        base.to_string()
    }
}

/// A random key for this run of the app, so an opaque workspace id cannot be
/// matched against guessed paths.
static ID_KEY: LazyLock<[u8; 16]> = LazyLock::new(rand::random);

/// The id viewers get for a workspace: the id itself when it carries no path
/// (a story id, `ops`), else `ws-` and 16 hex digits of a keyed hash, the same
/// in both lists for one run of the app, so `terminals` rows still join their
/// `workspaces` entry.
pub(crate) fn public_workspace_id(id: &str) -> String {
    if !has_path(id) {
        return id.to_string();
    }
    let mut hash = Sha256::new();
    hash.update(*ID_KEY);
    hash.update(id.as_bytes());
    let digest = hash.finalize();
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("ws-{hex}")
}

/// The `terminals` rows (AC-1, AC-9), in handle order, before redaction.
pub(crate) fn terminal_rows(
    rows: &[SessionListRow],
    team: &Team,
    host: &dyn AgentHost,
    names: &Names,
) -> Vec<Value> {
    rows.iter()
        .filter(|row| watchable(row, team, host))
        .take(catalog::MAX_ROWS)
        .map(|row| {
            let mut out = catalog::session_value(row, &[]);
            out["workspace"] = match &row.workspace {
                Some(id) => names.workspace(id, row.story.as_deref()),
                None => Value::Null,
            };
            out["watchable"] = json!(true);
            out
        })
        .collect()
}

/// The `workspaces` list (AC-10): every workspace of `team` in this Mac's live
/// layout, with the handles of its `terminals` rows and its tab count. The
/// workspace key (which names the org) is never in it.
pub(crate) fn workspace_list(
    tabs: &[LayoutTab],
    team: &Team,
    terminals: &[Value],
    names: &Names,
) -> Vec<Value> {
    use crate::commands::pty_layout::{team_key, WORKSPACE_KEY_SEPARATOR};
    let prefix = format!(
        "{}{WORKSPACE_KEY_SEPARATOR}",
        team_key(&team.org, Some(&team.team))
    );
    let mut open: BTreeMap<&str, (&str, usize)> = BTreeMap::new();
    for tab in tabs.iter().filter(|t| t.workspace_key.starts_with(&prefix)) {
        open.entry(tab.workspace_key.as_str())
            .or_insert((tab.workspace.as_str(), 0))
            .1 += 1;
    }
    open.into_values()
        .take(catalog::MAX_ROWS)
        .map(|(id, tabs)| {
            let mut out = names.workspace(id, None);
            // The rows carry the same public id (`Names::workspace` made both).
            let sessions: Vec<Value> = terminals
                .iter()
                .filter(|row| row["workspace"]["id"] == out["id"])
                .map(|row| row["handle"].clone())
                .collect();
            out["sessions"] = Value::Array(sessions);
            out["tabs"] = json!(tabs);
            out
        })
        .collect()
}

/// Which list a value stream sends.
enum List {
    Terminals,
    Workspaces,
    Inbox {
        ctx: LiveCtx,
        query: super::inbox::HostedQuery,
        fields: Vec<String>,
    },
}

pub(crate) fn open_inbox(
    ctx: LiveCtx,
    call: StreamCall,
) -> BoxFuture<'static, Result<Items, QueryError>> {
    Box::pin(async move {
        let query = super::inbox::args(call.args.clone())?;
        let host = host_of(&ctx)?;
        let team = scoped_team(&host, &call.folder, call.scope.as_ref()).await?;
        let fields = call.fields.clone();
        Ok(list_items(
            host,
            call,
            team,
            List::Inbox { ctx, query, fields },
        ))
    })
}

pub(crate) fn open_terminals(
    ctx: LiveCtx,
    call: StreamCall,
) -> BoxFuture<'static, Result<Items, QueryError>> {
    open_list(ctx, call, List::Terminals)
}

pub(crate) fn open_workspaces(
    ctx: LiveCtx,
    call: StreamCall,
) -> BoxFuture<'static, Result<Items, QueryError>> {
    open_list(ctx, call, List::Workspaces)
}

fn open_list(
    ctx: LiveCtx,
    call: StreamCall,
    list: List,
) -> BoxFuture<'static, Result<Items, QueryError>> {
    Box::pin(async move {
        let _: serde_json::Map<String, Value> = catalog::strict(call.args.clone(), &[])?;
        let host = host_of(&ctx)?;
        let team = scoped_team(&host, &call.folder, call.scope.as_ref()).await?;
        Ok(list_items(host, call, team, list))
    })
}

/// The state of one watched list stream.
struct ListState {
    host: Arc<dyn AgentHost>,
    folder: PathBuf,
    scope: Option<super::state::Scope>,
    team: Team,
    list: List,
    snap: Arc<tokio::sync::Notify>,
    /// The sha of the last value sent.
    sent: Option<[u8; 32]>,
    /// A read is due now (the first, or after a `snap`).
    due: bool,
    branches: Vec<WorktreeBranch>,
    branches_at: Option<Instant>,
    /// Passes in a row whose sessions (or layout) could not be read.
    failures: u8,
    /// Team checks in a row that failed: one (an index rebuilding) is not an end.
    team_watch: TeamWatch,
    ended: bool,
}

/// After this many failed passes in a row (about 4.5 s) a list ends `failed`; the
/// Workspaces page watches it again a little later.
pub(crate) const LIST_FAILURES: u8 = 3;

/// The one helper behind `terminals` and `workspaces`: read every
/// [`LIST_EVERY`], send the redacted value only when its sha changed (or on a
/// `snap`), end `not_found` when the folder's team changes.
fn list_items(host: Arc<dyn AgentHost>, call: StreamCall, team: Team, list: List) -> Items {
    let state = ListState {
        host,
        folder: call.folder,
        scope: call.scope,
        team,
        list,
        snap: call.snap,
        sent: None,
        due: true,
        branches: Vec::new(),
        branches_at: None,
        failures: 0,
        team_watch: TeamWatch::default(),
        ended: false,
    };
    futures_util::stream::unfold(state, |mut st| async move {
        if st.ended {
            return None;
        }
        loop {
            if !st.due {
                tokio::select! {
                    biased;
                    _ = st.snap.notified() => st.sent = None,
                    _ = tokio::time::sleep(LIST_EVERY) => {}
                }
            }
            st.due = false;
            let found = scoped_team(&st.host, &st.folder, st.scope.as_ref()).await;
            match st.team_watch.judge(found, &st.team) {
                TeamVerdict::Holds => {}
                TeamVerdict::Retry => continue,
                TeamVerdict::End(reason) => {
                    st.ended = true;
                    return Some((StreamItem::End(reason), st));
                }
            }
            let Some(value) = read_list(&mut st).await else {
                st.failures += 1;
                if st.failures >= LIST_FAILURES {
                    st.ended = true;
                    return Some((StreamItem::End("failed"), st));
                }
                continue;
            };
            st.failures = 0;
            let value = catalog::redact(value);
            let sha: [u8; 32] = Sha256::digest(value.to_string().as_bytes()).into();
            if st.sent == Some(sha) {
                continue;
            }
            st.sent = Some(sha);
            return Some((StreamItem::Value(value), st));
        }
    })
    .boxed()
}

/// One read of a list. `None` when the sessions (or, for workspaces, the layout)
/// could not be read: the pass is skipped rather than sending an empty list.
async fn read_list(st: &mut ListState) -> Option<Value> {
    if let List::Inbox { ctx, query, fields } = &st.list {
        // A failed read is retried by the same bounded loop as agent lists.
        // Never log the store error: it might carry message content.
        return super::inbox::read(ctx, &st.team, query, fields).await.ok();
    }
    let rows = match st.host.sessions().await {
        Ok(rows) => rows,
        Err(e) => {
            log::info!(target: "dashboard_publish", "live stream: sessions not read: {e}");
            return None;
        }
    };
    if st
        .branches_at
        .is_none_or(|at| at.elapsed() >= WORKTREES_EVERY)
    {
        // A failed read keeps the last branches; it is tried again next time.
        if let Some(branches) = st.host.worktrees().await {
            st.branches = branches;
            st.branches_at = Some(Instant::now());
        }
    }
    let tabs = match st.list {
        List::Workspaces => match st.host.layout().await {
            Some(tabs) => tabs,
            None => {
                log::info!(target: "dashboard_publish", "live stream: layout not read");
                return None;
            }
        },
        List::Terminals | List::Inbox { .. } => Vec::new(),
    };
    let host = st.host.as_ref();
    let names = Names::new(host, &st.team, &st.branches);
    let terminals = terminal_rows(&rows, &st.team, host, &names);
    Some(match st.list {
        List::Terminals => json!({ "terminals": terminals }),
        List::Workspaces => {
            json!({ "workspaces": workspace_list(&tabs, &st.team, &terminals, &names) })
        }
        List::Inbox { .. } => unreachable!("inbox was read above"),
    })
}

#[derive(serde::Deserialize)]
struct TerminalArgs {
    handle: String,
}

/// `terminal {handle}` (AC-3 to AC-6).
pub(crate) fn open_terminal(
    ctx: LiveCtx,
    call: StreamCall,
) -> BoxFuture<'static, Result<Items, QueryError>> {
    Box::pin(async move {
        let args: TerminalArgs = catalog::strict(call.args.clone(), &["handle"])?;
        if args.handle.is_empty() || args.handle.len() > MAX_HANDLE {
            return Err(QueryError::Refused("not_found"));
        }
        let host = host_of(&ctx)?;
        let team = scoped_team(&host, &call.folder, call.scope.as_ref()).await?;
        let session = find_watchable(&host, &args.handle, &team).await?;
        let watch = host
            .subscribe(session.clone())
            .await
            .map_err(QueryError::Failed)?;
        Ok(terminal_items(
            host,
            call,
            team,
            args.handle,
            session,
            watch,
        ))
    })
}

/// SW-DP-23: the session an owner's input for `handle` goes to, only while it
/// passes SW-DP-21's filter now: the dashboard's team (`scoped_team`) and a
/// watchable session of that team on this Mac (`find_watchable`). Any failure
/// is `refused`, whatever the reason.
pub(crate) async fn input_session(
    host: &Arc<dyn AgentHost>,
    folder: &Path,
    scope: Option<&super::state::Scope>,
    handle: &str,
) -> Result<String, &'static str> {
    if handle.is_empty() || handle.len() > MAX_HANDLE {
        return Err("refused");
    }
    let team = scoped_team(host, folder, scope)
        .await
        .map_err(|_| "refused")?;
    find_watchable(host, handle, &team)
        .await
        .map_err(|_| "refused")
}

/// The session behind `handle` when it is a watchable session of `team` (the
/// `terminals` filter for that one row). Otherwise `closed` for a bound session of
/// the team on this Mac that is Offline or listed with no tab for now (a page
/// retries it), and `not_found` for anything else, whatever the reason.
async fn find_watchable(
    host: &Arc<dyn AgentHost>,
    handle: &str,
    team: &Team,
) -> Result<String, QueryError> {
    let rows = host.sessions().await.map_err(QueryError::Failed)?;
    let Some(row) = rows.iter().find(|row| row.handle == handle) else {
        return Err(QueryError::Refused("not_found"));
    };
    if !watchable(row, team, host.as_ref()) {
        // Bound, in this team and on this Mac but Offline or listed with no tab for now (a
        // host restarting, a layout read that failed): `closed`, which a page retries.
        // Anything else, including another team's session, is `not_found`.
        let passing = row.machine_id.is_none()
            && row.state != SessionState::Exited
            && host.launch_team(handle).as_ref() == Some(team)
            && host.resolve(handle).is_some()
            && (row.state == SessionState::Offline || row.machine.is_none());
        return Err(QueryError::Refused(if passing {
            "closed"
        } else {
            "not_found"
        }));
    }
    host.resolve(handle).ok_or(QueryError::Refused("not_found"))
}

/// Unsubscribes when dropped: when the stream ends, is stopped (`u`, K32) or
/// its link drops, the app's stream goes too (AC-6).
struct Subscribed {
    host: Arc<dyn AgentHost>,
    handle: Option<WatchHandle>,
}

impl Drop for Subscribed {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.host.unsubscribe(handle);
        }
    }
}

/// How much of the visible text already sent is kept, to find a secret that
/// starts in it (a key whose value comes in later output).
const REDACT_TAIL: usize = 256;
/// The most visible text held back at the end of output: a run of value
/// characters that may be the start of a secret (`sk-ant-api` before the rest
/// comes). The next output or a flush sends it.
const REDACT_HOLD: usize = 256;
/// After a credential key and its separator, the most whitespace (one newline
/// at most) before its value. A value further away is not taken as that key's.
const KEY_WHITESPACE: u16 = 64;
/// Authorization scheme words, lowercase. After a credential key, one of these
/// followed by a space or tab is not the value: the credential comes after it
/// (`X-Auth-Token: Basic <base64>`). Both are masked.
const SCHEMES: &[&str] = &["basic", "bearer", "digest", "negotiate", "token"];
/// The most of a PEM key kept to find its END line across events.
const PEM_PROBE: usize = 64;
/// The longest escape sequence followed: longer, it is taken as ended.
const ESCAPE_MAX: usize = 4096;

/// Where the escape lexer is: escape sequences pass through unmasked and never
/// change what is recognised.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Escape {
    #[default]
    Idle,
    /// Just after ESC.
    Start,
    /// A CSI (`ESC [`), to its final byte.
    Csi,
    /// `ESC` and intermediates, to the final byte.
    Intermediate,
    /// An OSC, DCS, SOS, PM or APC string, to BEL or ST.
    Str,
    /// An ESC inside such a string: ST if `\` follows.
    StrEsc,
}

/// The sequences an escape introduces: the character after ESC, its 8-bit
/// (C1) form, and the state it starts. One table for both forms, the set the
/// emulator's own `SequenceTail` knows.
const INTRODUCERS: &[(char, char, Escape)] = &[
    ('[', '\u{9b}', Escape::Csi),
    (']', '\u{9d}', Escape::Str),
    ('P', '\u{90}', Escape::Str),
    ('X', '\u{98}', Escape::Str),
    ('^', '\u{9e}', Escape::Str),
    ('_', '\u{9f}', Escape::Str),
];
/// String Terminator: the character after ESC, and its C1 form.
const ST_AFTER_ESC: char = '\\';
const ST_C1: char = '\u{9c}';

/// The state `c` introduces, after ESC (`c1` false) or on its own as a C1
/// control (`c1` true).
fn introduced(c: char, c1: bool) -> Option<Escape> {
    Option::map(
        Iterator::find(
            &mut <[(char, char, Escape)]>::iter(INTRODUCERS),
            |&&(after_esc, alone, _)| c == if c1 { alone } else { after_esc },
        ),
        |&(_, _, state)| state,
    )
}

/// What the visible text at the current position is part of.
#[derive(Default, Debug)]
enum Mode {
    #[default]
    Normal,
    /// After a credential key and its separator (or an `Authorization:` scheme):
    /// whitespace and one quote pass; a value character starts the value.
    KeyPending {
        ws_left: u16,
        newline: bool,
        quoted: bool,
    },
    /// Inside a secret value: every value character is masked. After a
    /// credential key, `scheme` is the value so far (lowercase) while it may
    /// still be one of [`SCHEMES`].
    InValue { scheme: Option<String> },
    /// Inside a PEM key's body: masked through its END line.
    InPem { probe: String },
}

/// Where a secret was recognised in normal text.
#[derive(Clone, Copy)]
enum Hit {
    /// A value or token: masked from `at` to `end`, and on through value characters.
    Value { at: usize, end: usize },
    /// A key and its separator ending at `end`: its value follows.
    Key { end: usize },
    /// A PEM header ending at `end`: its body follows.
    Pem { end: usize },
}

/// SV-PBF-286: redacts a terminal's snapshot and output as one lexical stream.
///
/// Escape sequences pass through unmasked and are invisible to recognition. In
/// visible text the shared detectors (`secret_scan::STREAM_PATTERNS` and
/// `catalog::extra_tokens`) find where a secret starts; from there a mode masks
/// it character by character, for any length and across any number of events:
/// a token or value through its value characters, a key's value after up to
/// [`KEY_WHITESPACE`] of whitespace, a PEM key's body through its END line. The
/// key, the PEM header and the escapes stay visible. Only a run of value
/// characters at the end of output (at most [`REDACT_HOLD`]) is held back, so
/// a secret whose start is not yet recognisable is masked whole when its end
/// comes; a flush sends it. The snapshot runs through the same machine and
/// seeds the stream. A string escape's payload (a window title, a hyperlink) is
/// redacted on its own, whole.
///
/// A token is recognised at its prefix (`sk-`, `ghp_`, `eyJ` and eight more…),
/// so its start can never leave the lookback before it is whole.
///
/// What can still go out: the start of a secret sent by a flush before it
/// could be recognised (part of a token's prefix, or the first characters of a
/// bare `bearer` token, printed or typed slowly), and a value broken up by a control character (a carriage return or
/// backspace) or redrawn out of order. A value after a credential key is
/// masked whatever its length (`secret: it's` shows `secret: [redacted]'s`)
/// A scheme word after a credential key ([`SCHEMES`]) is masked with the
/// credential after it (`X-Auth-Token: Basic …` shows `X-Auth-Token: [redacted] [redacted]`).
#[derive(Default)]
struct TerminalRedactor {
    /// Lexed output not yet sent: each character and whether it is part of an
    /// escape sequence.
    pending: Vec<(char, bool)>,
    escape: Escape,
    escape_len: usize,
    /// The payload of the string escape in progress, and whether it went over
    /// [`ESCAPE_MAX`].
    payload: String,
    payload_over: bool,
    mode: Mode,
    /// The end of the visible text already sent.
    tail: String,
    /// How much of `tail` is normal text since the last secret ended: where
    /// recognition looks back from, as it would in one text.
    normal_len: usize,
    /// The last character sent was masked: a masked run goes on with no new
    /// placeholder, whatever the events.
    masking: bool,
}

impl TerminalRedactor {
    /// A redactor seeded from `vt` (a snapshot), and `vt` redacted.
    fn after_snapshot(vt: &str) -> (Self, String) {
        let mut redactor = Self::default();
        Self::lex(&mut redactor, vt);
        let vt = Self::release(&mut redactor, true);
        redactor.masking = false;
        (redactor, vt)
    }

    /// The output that can go now, redacted (empty when it is all held).
    fn push(&mut self, data: &str) -> String {
        Self::lex(self, data);
        Self::release(self, false)
    }

    /// Everything held, redacted.
    fn flush(&mut self) -> Option<String> {
        bool::then(Self::holding(self), || Self::release(self, true))
    }

    fn holding(&self) -> bool {
        !Vec::is_empty(&self.pending)
    }

    fn lex(&mut self, data: &str) {
        for c in str::chars(data) {
            if matches!(self.escape, Escape::Str | Escape::StrEsc) {
                Self::string_step(self, c);
            } else {
                let escaped = Self::escape_step(self, c);
                Vec::push(&mut self.pending, (c, escaped));
            }
        }
    }

    /// A character of an OSC, DCS, SOS, PM or APC string. Its payload (a
    /// window title, a hyperlink) is kept until the string ends, then sent
    /// redacted as one unit (`catalog::redact_str`), between its introducer and
    /// terminator as they were; a payload over [`ESCAPE_MAX`] is masked whole
    /// after its numeric parameter.
    fn string_step(&mut self, c: char) {
        match (self.escape, c) {
            (Escape::StrEsc, ST_AFTER_ESC) => {
                Self::end_string(self);
                Extend::extend(&mut self.pending, [('\x1b', true), (ST_AFTER_ESC, true)]);
            }
            (Escape::StrEsc, _) => {
                // ESC ended the string and starts a new sequence with `c`.
                Self::end_string(self);
                self.escape = Escape::Start;
                self.escape_len = 1;
                Vec::push(&mut self.pending, ('\x1b', true));
                let escaped = Self::escape_step(self, c);
                Vec::push(&mut self.pending, (c, escaped));
            }
            (_, '\x1b') => self.escape = Escape::StrEsc,
            (_, '\x07' | '\x18' | '\x1a' | ST_C1) => {
                Self::end_string(self);
                Vec::push(&mut self.pending, (c, true));
            }
            _ if String::len(&self.payload) < ESCAPE_MAX => String::push(&mut self.payload, c),
            _ => self.payload_over = true,
        }
    }

    /// Ends a string: its payload goes out redacted. An oversized payload keeps
    /// only its leading numeric parameter (`0;` of a title), which holds no secret.
    fn end_string(&mut self) {
        let payload = std::mem::take(&mut self.payload);
        let payload = if std::mem::take(&mut self.payload_over) {
            String::from(numeric_parameter(&payload)) + secret_scan::REDACTION_PLACEHOLDER
        } else {
            catalog::redact_str(&payload)
        };
        Extend::extend(
            &mut self.pending,
            Iterator::map(str::chars(&payload), |c| (c, true)),
        );
        self.escape = Escape::Idle;
    }

    /// True when `c` is part of an escape sequence.
    fn escape_step(&mut self, c: char) -> bool {
        use Escape::*;
        if self.escape == Idle {
            if let Some(state) = introduced(c, true) {
                self.escape = state;
                self.escape_len = 1;
                return true;
            }
            if c != '\x1b' {
                return false;
            }
            self.escape_len = 0;
        }
        self.escape_len += 1;
        self.escape = match (self.escape, c) {
            (Idle, _) => Start,
            (_, '\x18' | '\x1a') => Idle,
            (Start, _) => match introduced(c, false) {
                Some(state) => state,
                None if matches!(c, ' '..='/') => Intermediate,
                None => Idle,
            },
            (Csi, '@'..='~') => Idle,
            (Csi, '\x1b') => Start,
            (Csi, _) => Csi,
            (Intermediate, ' '..='/') => Intermediate,
            (Intermediate, _) => Idle,
            // Strings are lexed by `string_step`.
            (Str | StrEsc, _) => self.escape,
        };
        if self.escape_len >= ESCAPE_MAX {
            self.escape = Idle;
        }
        true
    }

    /// Sends what is pending: all of it, or all but a trailing run that may be
    /// the start of a secret.
    fn release(&mut self, all: bool) -> String {
        let visible: String = Iterator::collect(Iterator::filter_map(
            <[(char, bool)]>::iter(&self.pending),
            |&(c, escaped)| bool::then_some(!escaped, c),
        ));
        let from = String::len(&self.tail);
        let text = std::mem::take(&mut self.tail) + &visible;
        let (masked, hold) = Self::recognise(self, &text, from, all);
        let mut out = String::new();
        let mut at = from;
        let mut ranges = Iterator::peekable(<[(usize, usize)]>::iter(&masked));
        let mut sent = 0;
        for &(c, escaped) in &self.pending {
            if escaped {
                String::push(&mut out, c);
                self.masking = false;
            } else {
                if at >= hold {
                    break;
                }
                while Option::is_some(&std::iter::Peekable::next_if(&mut ranges, |&&(_, end)| {
                    end <= at
                })) {}
                if Option::is_some_and(std::iter::Peekable::peek(&mut ranges), |&&(start, _)| {
                    start <= at
                }) {
                    if !self.masking {
                        String::push_str(&mut out, secret_scan::REDACTION_PLACEHOLDER);
                    }
                    self.masking = true;
                } else {
                    String::push(&mut out, c);
                    self.masking = false;
                }
                at += char::len_utf8(c);
            }
            sent += 1;
        }
        Vec::drain(&mut self.pending, ..sent);
        self.tail = String::from(tail(&text[..hold], REDACT_TAIL));
        out
    }

    /// Runs the modes over `text[from..]` (`text[..from]` was sent): the byte
    /// ranges to mask, in order, and where held text starts.
    fn recognise(&mut self, text: &str, from: usize, all: bool) -> (Vec<(usize, usize)>, usize) {
        let mut masked: Vec<(usize, usize)> = Vec::new();
        let mut mask = |start: usize, end: usize| match <[(usize, usize)]>::last_mut(&mut masked) {
            Some(last) if last.1 >= start => last.1 = usize::max(last.1, end),
            _ => Vec::push(&mut masked, (start, end)),
        };
        let mut p = from;
        // Normal text is searched from where it began, then from where it resumes;
        // every pattern searches the batch once, from where it began.
        let mut scan = from - usize::min(self.normal_len, from);
        let hits = candidates(text, scan);
        let mut next = 0;
        while p < str::len(text) {
            let c = Option::expect(
                Iterator::next(&mut str::chars(&text[p..])),
                "p is on a character",
            );
            match &mut self.mode {
                Mode::Normal => match next_hit(&hits, &mut next, scan, p) {
                    None => {
                        let hold = if all {
                            str::len(text)
                        } else {
                            held_run(text, p)
                        };
                        self.normal_len = hold - scan;
                        return (masked, hold);
                    }
                    Some(Hit::Value { at, end }) => {
                        mask(usize::max(at, p), end);
                        p = end;
                        self.mode = Mode::InValue { scheme: None };
                    }
                    Some(Hit::Key { end }) => {
                        p = end;
                        self.mode = Mode::KeyPending {
                            ws_left: KEY_WHITESPACE,
                            newline: false,
                            quoted: false,
                        };
                    }
                    Some(Hit::Pem { end }) => {
                        p = end;
                        self.mode = Mode::InPem {
                            probe: String::new(),
                        };
                    }
                },
                Mode::KeyPending {
                    ws_left,
                    newline,
                    quoted,
                } => {
                    if char::is_whitespace(c) && *ws_left > 0 && !(c == '\n' && *newline) {
                        *ws_left -= 1;
                        *newline |= c == '\n';
                        p += char::len_utf8(c);
                    } else if c == '"' && !*quoted {
                        *quoted = true;
                        p += 1;
                    } else if secret_scan::value_head(c) {
                        self.mode = Mode::InValue {
                            scheme: Some(String::new()),
                        };
                    } else {
                        self.mode = Mode::Normal;
                        scan = p;
                    }
                }
                Mode::InValue { scheme } => {
                    if secret_scan::value_tail(c) {
                        mask(p, p + char::len_utf8(c));
                        p += char::len_utf8(c);
                        if let Some(word) = Option::as_mut(scheme) {
                            String::push(word, char::to_ascii_lowercase(&c));
                            if !Iterator::any(&mut <[&str]>::iter(SCHEMES), |s| {
                                str::starts_with(s, String::as_str(word))
                            }) {
                                *scheme = None;
                            }
                        }
                    } else if matches!(c, ' ' | '\t')
                        && Option::is_some_and(Option::as_deref(scheme), |word| {
                            <[&str]>::contains(SCHEMES, &word)
                        })
                    {
                        // A scheme word, not the value: the credential follows it.
                        self.mode = Mode::KeyPending {
                            ws_left: KEY_WHITESPACE,
                            newline: false,
                            quoted: true,
                        };
                    } else {
                        self.mode = Mode::Normal;
                        scan = p;
                    }
                }
                Mode::InPem { probe } => {
                    if !secret_scan::pem_body(c) {
                        self.mode = Mode::Normal;
                        scan = p;
                        continue;
                    }
                    mask(p, p + char::len_utf8(c));
                    p += char::len_utf8(c);
                    String::push(probe, c);
                    if String::len(probe) > PEM_PROBE {
                        *probe = String::from(tail(probe, PEM_PROBE));
                    }
                    if regex::Regex::is_match(&secret_scan::STREAM_PATTERNS.pem_end, probe) {
                        self.mode = Mode::Normal;
                        scan = p;
                    }
                }
            }
        }
        self.normal_len = match self.mode {
            Mode::Normal => str::len(text) - scan,
            _ => 0,
        };
        (masked, str::len(text))
    }
}

/// Every place in `text` from `scan` on where a secret, a key or a PEM header
/// is recognised, sorted by where it starts (a key, then a PEM header, then a
/// value, at the same start). Each pattern searches the text once.
fn candidates(text: &str, scan: usize) -> Vec<(usize, u8, Hit)> {
    let patterns = &*secret_scan::STREAM_PATTERNS;
    let mut found: Vec<(usize, u8, Hit)> = Vec::new();
    let mut search = |re: &regex::Regex, rank: u8, hit: fn(regex::Match<'_>) -> Hit| {
        count_pattern_search();
        let mut at = scan;
        while let Some(m) = regex::Regex::find_at(re, text, at) {
            Vec::push(&mut found, (regex::Match::start(&m), rank, hit(m)));
            at = regex::Match::end(&m);
        }
    };
    for re in &patterns.keys {
        search(re, 0, |m| Hit::Key {
            end: regex::Match::end(&m),
        });
    }
    search(&patterns.pem_begin, 1, |m| Hit::Pem {
        end: regex::Match::end(&m),
    });
    for re in Iterator::chain(
        <[regex::Regex]>::iter(&patterns.tokens),
        [catalog::extra_tokens()],
    ) {
        search(re, 2, |m| Hit::Value {
            at: regex::Match::start(&m),
            end: regex::Match::end(&m),
        });
    }
    // A bearer token counts from its value (`next_hit`), but its `bearer ` can start
    // in text already sent, even inside a masked key value (`X-Auth-Token: B|earer …`),
    // so it is searched for from the start of the sent tail.
    count_pattern_search();
    let mut at = 0;
    while let Some(caps) = regex::Regex::captures_at(&patterns.bearer, text, at) {
        let whole = Option::expect(regex::Captures::get(&caps, 0), "match");
        let value = Option::expect(regex::Captures::name(&caps, "val"), "val");
        let hit = Hit::Value {
            at: regex::Match::start(&value),
            end: regex::Match::end(&whole),
        };
        Vec::push(&mut found, (regex::Match::start(&whole), 2, hit));
        at = regex::Match::end(&whole);
    }
    <[(usize, u8, Hit)]>::sort_by_key(&mut found, |&(start, rank, _)| (start, rank));
    found
}

#[cfg(test)]
thread_local! {
    /// Pattern searches on this thread, for the row 10 bound.
    pub(crate) static PATTERN_SEARCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_pattern_search() {
    std::thread::LocalKey::with(&PATTERN_SEARCHES, |n| {
        std::cell::Cell::set(n, std::cell::Cell::get(n) + 1)
    });
}

#[cfg(not(test))]
fn count_pattern_search() {}

/// The first of `hits` (from `next` on) that starts at or after `scan` and runs
/// past `p`. Those before it never will again (`scan` and `p` only grow), so
/// `next` moves past them.
fn next_hit(hits: &[(usize, u8, Hit)], next: &mut usize, scan: usize, p: usize) -> Option<Hit> {
    while let Some(&(start, _, hit)) = <[(usize, u8, Hit)]>::get(hits, *next) {
        let (begin, end) = match hit {
            // A bearer token counts from where its value starts: the key before
            // it may already have been masked up to `scan` (`X-Auth-Token: Bearer ...`).
            Hit::Value { at, end } => (at, end),
            Hit::Key { end } | Hit::Pem { end } => (start, end),
        };
        if begin >= scan && end > p {
            return Some(hit);
        }
        *next += 1;
    }
    None
}

/// Where a trailing run of value characters in `text[p..]` starts (at most
/// [`REDACT_HOLD`] back): it may be the start of a secret.
fn held_run(text: &str, p: usize) -> usize {
    let run = Iterator::take_while(Iterator::rev(str::char_indices(&text[p..])), |&(_, c)| {
        secret_scan::value_tail(c)
    });
    let run = Option::map_or(Iterator::last(run), str::len(text), |(i, _)| p + i);
    let mut hold = usize::max(run, usize::saturating_sub(str::len(text), REDACT_HOLD));
    while !str::is_char_boundary(text, hold) {
        hold += 1;
    }
    hold
}

/// A string escape's leading numeric parameter and its `;` (`0;`, `52;`), or
/// nothing.
fn numeric_parameter(payload: &str) -> &str {
    let digits = Iterator::count(Iterator::take_while(
        str::bytes(payload),
        u8::is_ascii_digit,
    ));
    match (<[u8]>::get(str::as_bytes(payload), digits), digits) {
        (Some(&b';'), 1..=4) => &payload[..=digits],
        _ => "",
    }
}

/// The last `max` bytes of `text`, from a character boundary.
fn tail(text: &str, max: usize) -> &str {
    let mut at = usize::saturating_sub(str::len(text), max);
    while !str::is_char_boundary(text, at) {
        at += 1;
    }
    &text[at..]
}

struct TermState {
    host: Arc<dyn AgentHost>,
    folder: PathBuf,
    scope: Option<super::state::Scope>,
    team: Team,
    handle: String,
    session: String,
    snap: Arc<tokio::sync::Notify>,
    sub: Subscribed,
    events: tokio::sync::mpsc::UnboundedReceiver<TermEvent>,
    /// The stream position shown so far: output at or before it is in the snapshot.
    seq: u64,
    pending: Option<TerminalSnap>,
    check_at: Instant,
    /// Team checks in a row that failed (two end the stream).
    team_watch: TeamWatch,
    /// Checks in a row whose session list had no row for the handle, or an Offline
    /// one (two end the stream `closed`).
    row_misses: u8,
    redactor: TerminalRedactor,
    /// When held output goes if no more comes.
    flush_at: Instant,
    /// An item that goes before anything else (the one after a flush).
    next: Option<StreamItem>,
    ended: bool,
}

/// The snapshot item, redacted before its K30 cut; the redactor starts again
/// from its end.
fn snapshot_item(snap: TerminalSnap, redactor: &mut TerminalRedactor) -> StreamItem {
    let (seeded, vt) = TerminalRedactor::after_snapshot(&snap.vt);
    *redactor = seeded;
    StreamItem::Snapshot {
        vt: cut_snapshot(&vt, SNAPSHOT_LINES, SNAPSHOT_JSON_BYTES),
        cols: Some(snap.cols),
        rows: Some(snap.rows),
    }
}

fn terminal_items(
    host: Arc<dyn AgentHost>,
    call: StreamCall,
    team: Team,
    handle: String,
    session: String,
    watch: TerminalWatch,
) -> Items {
    let state = TermState {
        sub: Subscribed {
            host: host.clone(),
            handle: Some(watch.handle),
        },
        host,
        folder: call.folder,
        scope: call.scope,
        team,
        handle,
        session,
        snap: call.snap,
        events: watch.events,
        seq: watch.snapshot.seq,
        pending: Some(watch.snapshot),
        check_at: Instant::now() + TEAM_CHECK_EVERY,
        team_watch: TeamWatch::default(),
        row_misses: 0,
        redactor: TerminalRedactor::default(),
        flush_at: Instant::now(),
        next: None,
        ended: false,
    };
    futures_util::stream::unfold(state, |mut st| async move {
        if let Some(item) = Option::take(&mut st.next) {
            return Some((item, st));
        }
        if st.ended {
            return None;
        }
        // Held output goes before `item`.
        let flushed = |mut st: TermState, item: StreamItem| match TerminalRedactor::flush(&mut st.redactor) {
            Some(held) => {
                st.next = Some(item);
                Some((StreamItem::Bytes(held), st))
            }
            None => Some((item, st)),
        };
        let end = |mut st: TermState, reason: &'static str| {
            st.ended = true;
            flushed(st, StreamItem::End(reason))
        };
        if let Some(snap) = st.pending.take() {
            let item = snapshot_item(snap, &mut st.redactor);
            return Some((item, st));
        }
        loop {
            tokio::select! {
                biased;
                _ = st.snap.notified() => {
                    // A fresh snapshot (a relay `snap`, or the link's own gap): subscribe
                    // again, then let the old subscription go, so nothing falls between.
                    match st.host.subscribe(st.session.clone()).await {
                        Ok(watch) => {
                            st.sub = Subscribed { host: st.host.clone(), handle: Some(watch.handle) };
                            st.events = watch.events;
                            st.seq = watch.snapshot.seq;
                            // Held output is in the fresh snapshot.
                            let item = snapshot_item(watch.snapshot, &mut st.redactor);
                            return Some((item, st));
                        }
                        Err(e) => {
                            log::info!(target: "dashboard_publish", "live terminal: resubscribe failed: {e}");
                            return end(st, "closed");
                        }
                    }
                }
                event = st.events.recv() => match event {
                    Some(TermEvent::Output { data, seq }) => {
                        // The app's stream delivers past the snapshot only; a repeat is dropped.
                        if seq <= st.seq || data.is_empty() {
                            continue;
                        }
                        st.seq = seq;
                        st.flush_at = Instant::now() + super::live::STREAM_FRAME_EVERY;
                        let data = TerminalRedactor::push(&mut st.redactor, &data);
                        if String::is_empty(&data) {
                            continue;
                        }
                        return Some((StreamItem::Bytes(data), st));
                    }
                    Some(TermEvent::Resized { cols, rows }) => {
                        return flushed(st, StreamItem::Resized { cols, rows });
                    }
                    Some(TermEvent::Exited) => return end(st, "exited"),
                    None => return end(st, "closed"),
                },
                _ = tokio::time::sleep_until(st.flush_at), if TerminalRedactor::holding(&st.redactor) => {
                    if let Some(held) = TerminalRedactor::flush(&mut st.redactor) {
                        return Some((StreamItem::Bytes(held), st));
                    }
                }
                _ = tokio::time::sleep_until(st.check_at) => {
                    st.check_at = Instant::now() + TEAM_CHECK_EVERY;
                    let found = scoped_team(&st.host, &st.folder, st.scope.as_ref()).await;
                    match st.team_watch.judge(found, &st.team) {
                        TeamVerdict::Holds => {}
                        TeamVerdict::Retry => continue,
                        TeamVerdict::End(reason) => return end(st, reason),
                    }
                    if st.host.launch_team(&st.handle).as_ref() != Some(&st.team) {
                        return end(st, "not_found");
                    }
                    if st.host.resolve(&st.handle).as_deref() != Some(st.session.as_str()) {
                        return end(st, "exited");
                    }
                    // An exit missed between the open's check and the exit listener: the list says
                    // so. A row missing from one read is not taken as an exit.
                    if let Ok(rows) = st.host.sessions().await {
                        match rows.iter().find(|r| r.handle == st.handle) {
                            Some(r) if r.state == SessionState::Exited => return end(st, "exited"),
                            // Moved to another machine: final, as at the open (the team was checked above).
                            Some(r) if r.machine_id.is_some() => {
                                return end(st, "not_found");
                            }
                            // On this Mac in the team: still there. A row with no tab is not judged
                            // here (a layout read that failed lists every row with none); a closed
                            // tab exits its session, which ends the stream.
                            Some(r) if r.state != SessionState::Offline => st.row_misses = 0,
                            // Gone or Offline in two reads in a row (a failed read counts neither
                            // way): `closed`, which a page may retry; a reopen of a session that is
                            // really gone gets `not_found`.
                            _ => {
                                st.row_misses += 1;
                                if st.row_misses >= 2 {
                                    return end(st, "closed");
                                }
                            }
                        }
                    }
                }
            }
        }
    })
    .boxed()
}

/// K30: `vt` cut to its most recent `max_lines` lines and at most `max_json`
/// bytes as JSON. A cut snapshot starts at a line, after an attribute reset;
/// one line too long on its own keeps its end, outside any escape sequence (an
/// unclosed one at the very end keeps nothing, as a terminal would show nothing).
pub(crate) fn cut_snapshot(vt: &str, max_lines: usize, max_json: usize) -> String {
    use super::live::{json_char_len, json_len};
    let starts: Vec<usize> = std::iter::once(0)
        .chain(
            vt.match_indices('\n')
                .map(|(i, _)| i + 1)
                .filter(|&i| i < vt.len()),
        )
        .collect();
    let lines = starts.len();
    let first = lines.saturating_sub(max_lines);
    if first == 0 && json_len(vt) <= max_json {
        return vt.to_string();
    }
    // The JSON size of each kept line, from the end, until the budget is spent.
    let budget = max_json.saturating_sub(json_len(RESET));
    let mut size = 0;
    let mut from = None;
    for i in (first..lines).rev() {
        let end = starts.get(i + 1).copied().unwrap_or(vt.len());
        let line = json_len(&vt[starts[i]..end]);
        if size + line > budget {
            break;
        }
        size += line;
        from = Some(starts[i]);
    }
    let from = match from {
        Some(at) => at,
        None => {
            // The last line alone is over the budget: keep as much of its end as fits.
            let mut size = 0;
            let mut at = vt.len();
            for (i, c) in vt.char_indices().rev() {
                size += json_char_len(c);
                if size > budget {
                    break;
                }
                at = i;
            }
            outside_escape(vt, at)
        }
    };
    format!("{RESET}{}", &vt[from..])
}

/// `at`, or the end of the escape sequence `at` falls inside (CSI, OSC, DCS
/// and other strings, or an ESC with intermediates), so a cut never starts
/// with a fragment of one. The sequence state is read from the last line start
/// up to 4 KB before `at` (a line feed ends every sequence but a string).
pub(crate) fn outside_escape(vt: &str, at: usize) -> usize {
    #[derive(Clone, Copy, PartialEq)]
    enum St {
        Ground,
        Esc,
        Csi,
        Str,
        StrEsc,
    }
    let bytes = vt.as_bytes();
    let floor = at.saturating_sub(4096);
    let start = bytes[floor..at]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(floor, |i| floor + i + 1);
    let step = |st: St, b: u8| -> St {
        match st {
            St::Ground => {
                if b == 0x1b {
                    St::Esc
                } else {
                    St::Ground
                }
            }
            St::Esc => match b {
                0x1b => St::Esc,
                b'[' => St::Csi,
                b']' | b'P' | b'X' | b'^' | b'_' => St::Str,
                // Intermediates (`ESC ( B`, `ESC # 8`) wait for their final byte.
                0x20..=0x2f => St::Esc,
                _ => St::Ground,
            },
            // ESC starts a new sequence and CAN or SUB cancel the CSI.
            St::Csi => match b {
                0x1b => St::Esc,
                0x18 | 0x1a | 0x40..=0x7e => St::Ground,
                _ => St::Csi,
            },
            St::Str => match b {
                0x07 | 0x18 | 0x1a => St::Ground,
                0x1b => St::StrEsc,
                _ => St::Str,
            },
            // ST ends the string; any other ESC ends it too and starts a new sequence.
            St::StrEsc => match b {
                b'\\' | 0x18 | 0x1a => St::Ground,
                b'[' => St::Csi,
                b']' | b'P' | b'X' | b'^' | b'_' => St::Str,
                0x20..=0x2f | 0x1b => St::Esc,
                _ => St::Ground,
            },
        }
    };
    let mut st = St::Ground;
    for &b in &bytes[start..at] {
        st = step(st, b);
    }
    let mut to = at;
    while st != St::Ground && to < bytes.len() {
        // A CSI cut short by a new ESC ends there: the new sequence is kept whole.
        if st == St::Csi && bytes[to] == 0x1b {
            st = St::Ground;
            break;
        }
        // So does a string ended by an ESC that starts a new sequence (not ST), when that
        // ESC is at or after `at` (the cut never moves back past where it was asked for).
        if st == St::StrEsc
            && to > at
            && matches!(
                bytes[to],
                b'[' | b']' | b'P' | b'X' | b'^' | b'_' | 0x1b | 0x20..=0x2f
            )
        {
            to -= 1;
            st = St::Ground;
            break;
        }
        st = step(st, bytes[to]);
        to += 1;
    }
    if st != St::Ground {
        // A sequence never closed: start at the next line, if there is one.
        // With no line after it, everything left is inside the sequence: none of it is kept
        // (a terminal would show none of it either).
        to = bytes[at..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(vt.len(), |i| at + i + 1);
    }
    while to < vt.len() && !vt.is_char_boundary(to) {
        to += 1;
    }
    to
}

// ---- The app ----

/// The team whose context root holds `folder` in the index's current snapshot, and that
/// snapshot's readiness, read once.
pub(crate) fn index_team(
    service: &crate::context_query::ContextIndexService,
    folder: &Path,
) -> TeamLookup {
    let snapshot = service.snapshot();
    TeamLookup {
        team: catalog::team_of(&snapshot.roots, folder).map(|(org, team)| Team { org, team }),
        ready: snapshot.ready,
    }
}

/// The app as the agent streams read it.
pub(crate) struct AppAgents {
    pub app: tauri::AppHandle,
}

impl AppAgents {
    /// The data home and the selected local profile.
    fn home_and_profile(&self) -> Option<(std::path::PathBuf, crate::commands::local_profile::LocalProfile)> {
        Some((
            crate::commands::paths::codesense_home_dir()?,
            crate::commands::local_profile::peek_active(&self.app)?,
        ))
    }
}

/// A `pty-output-stream-{id}` payload as a [`TermEvent`].
pub(crate) fn stream_event(payload: &Value) -> Option<TermEvent> {
    if let Some(resized) = payload.get("resized") {
        let dim = |k: &str| {
            resized
                .get(k)
                .and_then(Value::as_u64)
                .and_then(|n| u16::try_from(n).ok())
        };
        return Some(TermEvent::Resized {
            cols: dim("cols")?,
            rows: dim("rows")?,
        });
    }
    Some(TermEvent::Output {
        data: payload.get("data")?.as_str()?.to_string(),
        seq: payload.get("seq")?.as_u64()?,
    })
}

impl AgentHost for AppAgents {
    fn agent_avatar(&self, key: &str) -> Option<String> {
        let (home, profile) = self.home_and_profile()?;
        crate::agents::service::list(&home, &profile.principal, true)
            .ok()?
            .into_iter()
            .find(|found| found.key.to_string() == key)?
            .agent
            .avatar
    }

    fn human_avatar(&self) -> Option<String> {
        crate::commands::local_profile::peek_active(&self.app)?.identity_avatar
    }

    fn default_pool(&self, team: &Team) -> Option<Vec<String>> {
        let (home, profile) = self.home_and_profile()?;
        let roots = crate::avatars::store::roots_for_team(&home, &profile.principal, &team.org, &team.team);
        let default = roots
            .iter()
            .find(|r| r.scope == crate::avatars::PackScope::Team)
            .and_then(|r| crate::avatars::store::read_default_pack(&r.avatars_dir))?;
        let refs = if default.starts_with(crate::avatars::BUILTIN_PREFIX) {
            crate::avatars::pool::builtin_refs(&default)
        } else {
            let found = crate::avatars::store::discover(&roots);
            let pack = found.packs.into_iter().find(|p| p.id == default)?;
            pack.faces
                .iter()
                .filter(|face| crate::avatars::store::display_source(face).is_ok())
                .map(|face| crate::avatars::face_ref(&pack.id, &face.id))
                .collect()
        };
        (!refs.is_empty()).then_some(refs)
    }

    fn avatar_face(
        &self,
        team: &Team,
        pack: &str,
        id: &str,
    ) -> Option<(crate::avatars::FaceFormat, Vec<u8>)> {
        let (home, profile) = self.home_and_profile()?;
        let roots = crate::avatars::store::roots_for_team(&home, &profile.principal, &team.org, &team.team);
        let face = crate::avatars::store::find_face(&roots, pack, id)?;
        crate::avatars::store::source_bytes(&face).ok()
    }

    fn folder_team(&self, folder: &Path) -> TeamLookup {
        use tauri::Manager;
        match self.app.try_state::<crate::context_query::ContextIndexService>() {
            Some(service) => index_team(&service, folder),
            None => TeamLookup { team: None, ready: false },
        }
    }

    fn sessions(&self) -> BoxFuture<'static, Result<Vec<SessionListRow>, String>> {
        let app = self.app.clone();
        Box::pin(async move { crate::mcp::session_list::read_session_list(&app, None).await })
    }

    fn launch_team(&self, handle: &str) -> Option<Team> {
        use tauri::Manager;
        self.app
            .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()?
            .launch_team(handle)
            .map(|t| Team {
                org: t.org,
                team: t.team,
            })
    }

    fn resolve(&self, handle: &str) -> Option<String> {
        use tauri::Manager;
        self.app
            .try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()?
            .resolve(handle)
    }

    fn layout(&self) -> BoxFuture<'static, Option<Vec<LayoutTab>>> {
        let app = self.app.clone();
        Box::pin(async move {
            let handle = tokio::runtime::Handle::current();
            crate::commands::await_blocking("live.workspaces", move || {
                let live = handle.block_on(crate::mcp::terminal_tabs::LiveTabs::read_local(&app));
                Ok(live
                    .tabs
                    .into_iter()
                    .filter(|t| t.machine_id == crate::commands::machines::LOCAL_MACHINE_ID)
                    .map(|t| LayoutTab {
                        workspace_key: t.workspace_key,
                        workspace: t.workspace,
                    })
                    .collect())
            })
            .await
            .ok()
        })
    }

    fn worktrees(&self) -> BoxFuture<'static, Option<Vec<WorktreeBranch>>> {
        let app = self.app.clone();
        Box::pin(async move {
            let handle = tokio::runtime::Handle::current();
            crate::commands::await_blocking("live.worktrees", move || {
                let listing = handle.block_on(crate::mcp::worktree_tools::list_worktrees(&app))?;
                Ok(listing
                    .rows
                    .into_iter()
                    .filter_map(|r| {
                        Some(WorktreeBranch {
                            story: r.story_id.filter(|s| !s.is_empty())?,
                            org: r.org_slug,
                            team: r.team,
                            branch: r.branch,
                        })
                    })
                    .collect())
            })
            .await
            .ok()
        })
    }

    fn story_title(&self, team: &Team, id: &str) -> Option<String> {
        use tauri::Manager;
        let service = self
            .app
            .try_state::<crate::context_query::ContextIndexService>()?;
        let snapshot = service.snapshot();
        snapshot
            .records
            .iter()
            .find(|r| {
                r.layer == crate::context_query::model::Layer::Team
                    && r.is_pipeline_doc
                    && r.org.as_deref() == Some(team.org.as_str())
                    && r.team.as_deref() == Some(team.team.as_str())
                    && r.fields.get("story_id").and_then(Value::as_str) == Some(id)
            })
            .map(|r| r.title.clone().unwrap_or_else(|| id.to_string()))
    }

    fn subscribe(&self, session: String) -> BoxFuture<'static, Result<TerminalWatch, String>> {
        let app = self.app.clone();
        Box::pin(async move {
            // The subscribe runs in a task of its own: the blocking subscribe cannot be
            // cancelled, so when the caller goes away (a `u` during the open, the open's
            // timeout, the link dropping) the task undoes what it made instead of leaking it.
            let (done, got) = tokio::sync::oneshot::channel();
            let task_app = app.clone();
            tauri::async_runtime::spawn(async move {
                let result = subscribe_now(task_app.clone(), session).await;
                hand_over(done, result, |handle| {
                    AppAgents { app: task_app }.unsubscribe(handle)
                });
            });
            // A watch handed over just as this caller went away is undone too.
            let mut pending = Pending {
                got: Some(got),
                undo: Some(Box::new(move |handle| {
                    AppAgents { app }.unsubscribe(handle)
                })),
            };
            let got = pending.got.as_mut().expect("set above");
            let result = got.await;
            pending.got = None;
            result.map_err(|_| "the terminal subscribe stopped".to_string())?
        })
    }

    fn write(&self, session: String, data: String) -> BoxFuture<'static, Result<(), String>> {
        let app = self.app.clone();
        let reference = crate::commands::machines::RuntimeSessionReference::Legacy(session);
        Box::pin(crate::commands::pty::submit_automatic_input(
            app.clone(),
            reference.clone(),
            crate::commands::pty::write_session_input(
                app,
                reference,
                data,
                false,
                crate::commands::pty::HumanInput::Typed,
            ),
        ))
    }

    fn resize(
        &self,
        session: String,
        cols: u16,
        rows: u16,
    ) -> BoxFuture<'static, Result<(), String>> {
        let app = self.app.clone();
        Box::pin(crate::commands::pty::resize_session_as(
            app,
            crate::commands::machines::RuntimeSessionReference::Legacy(session),
            codesense_pty_core::domain::ViewerId::new(VIEWPORT),
            cols,
            rows,
            false,
        ))
    }

    fn unsubscribe(&self, handle: WatchHandle) {
        use tauri::Listener;
        for id in &handle.listeners {
            self.app.unlisten(*id);
        }
        let app = self.app.clone();
        // `unsubscribe_terminal`'s work, off the caller (it may be a dropped task): a
        // sidecar-hosted stream is detached over its socket. Releases a reserved id too.
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(e) = crate::commands::pty_backend::unsubscribe(&app, handle.stream) {
                log::info!(target: "dashboard_publish", "live terminal: unsubscribe failed: {e}");
            }
        });
    }
}

/// The caller's side of a subscribe in flight: dropped before it read the result
/// (its open was cancelled), it closes the channel and undoes a watch already
/// handed over; one handed over later is undone by [`hand_over`].
pub(crate) struct Pending {
    pub(crate) got: Option<tokio::sync::oneshot::Receiver<Result<TerminalWatch, String>>>,
    pub(crate) undo: Option<Box<dyn FnOnce(WatchHandle) + Send>>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(mut got) = self.got.take() {
            got.close();
            if let (Ok(Ok(watch)), Some(undo)) = (got.try_recv(), self.undo.take()) {
                undo(watch.handle);
            }
        }
    }
}

/// Hands a finished subscribe to its caller, or undoes it when the caller has
/// gone (its open was cancelled or timed out), so nothing is left subscribed.
pub(crate) fn hand_over(
    done: tokio::sync::oneshot::Sender<Result<TerminalWatch, String>>,
    result: Result<TerminalWatch, String>,
    undo: impl FnOnce(WatchHandle),
) {
    if let Err(Ok(watch)) = done.send(result) {
        undo(watch.handle);
    }
}

/// Subscribe the `dashboard` viewport to `session`: reserve the app's stream id,
/// listen on its event and on `pty-exit`, then subscribe as the app's panes do.
async fn subscribe_now(app: tauri::AppHandle, session: String) -> Result<TerminalWatch, String> {
    use tauri::Listener;
    let stream = crate::commands::pty::reserve_terminal_stream(app.clone())?;
    let (tx, events) = tokio::sync::mpsc::unbounded_channel();
    let out = tx.clone();
    let output = app.listen(
        crate::commands::pty_host::stream_event_name(stream),
        move |event| {
            if let Some(e) = serde_json::from_str::<Value>(event.payload())
                .ok()
                .as_ref()
                .and_then(stream_event)
            {
                let _ = out.send(e);
            }
        },
    );
    let mine = session.clone();
    let exit = app.listen("pty-exit", move |event| {
        let payload: Value = serde_json::from_str(event.payload()).unwrap_or(Value::Null);
        if payload.get("session_id").and_then(Value::as_str) == Some(mine.as_str()) {
            let _ = tx.send(TermEvent::Exited);
        }
    });
    let handle = WatchHandle {
        stream,
        listeners: vec![output, exit],
    };
    let subscribing = app.clone();
    let subscribed = crate::commands::await_blocking("live.terminal", move || {
        crate::commands::pty_backend::subscribe_reference_authorized(
            &subscribing,
            &crate::commands::machines::RuntimeSessionReference::Legacy(session),
            &codesense_pty_core::domain::ViewportId::new(VIEWPORT),
            Some(stream),
            false,
        )
    })
    .await;
    match subscribed {
        Ok(result) => Ok(TerminalWatch {
            snapshot: TerminalSnap {
                cols: result.snapshot.cols,
                rows: result.snapshot.rows,
                seq: result.snapshot.seq,
                vt: result.snapshot.vt,
            },
            handle,
            events,
        }),
        Err(e) => {
            AppAgents { app }.unsubscribe(handle);
            Err(e)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
