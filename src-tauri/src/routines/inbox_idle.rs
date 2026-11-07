// SPDX-License-Identifier: MIT
//! The `inbox_idle` event trigger: a routine that fires when an agent sits idle at its
//! prompt with inbox mail it has not read (`{kind: inbox_idle}`).
//!
//! Supported Claude and Codex sessions receive native inbox notices without a routine,
//! through Claude's waiter or Codex's bound engine and checks. A session with no ready
//! native path, or one that leaves its mail unread, can still fire these optional routines:
//!
//! - a `wake` routine (SW-AGR-2) is fired only by its own agent's sessions, and types
//!   its task into the one with the mail (`wake`); another agent's mail never fires it;
//! - a `fresh` routine is fired by any session in its reach, and starts its agent, whose
//!   task decides what to do with `{{trigger.sessions}}`.
//!
//! Sessions are offered here by:
//!
//! - a message stored for it (`agent_inbox::store_message`);
//! - one sweep a while after the app starts, for mail that arrived while it was closed.
//!
//! Offers are collected for [`SETTLE`] and then checked against the live session list:
//! only sessions that list `idle` after a clean turn (not one that errored or crashed)
//! with unread mail go on. One that is still busy, or whose state a remote relay has not
//! reported yet, is looked at again up to [`RECHECKS`] more times.
//!
//! Guards, all on this Mac and in memory (a restart seeds the window from the run log):
//!
//! - A routine fires at most once per [`WINDOW_MS`], and never while its last run is
//!   still queued or running: sessions that arrive meanwhile wait for the next fire,
//!   batched, and write no run row.
//! - A session is offered to a routine once per message (again only when newer mail
//!   arrives), plus once more after [`REOFFER_MS`] if that mail is still unread, in case
//!   the wake did not land.
//!
//! A wake routine stops after [`MAX_IGNORED_WAKES`] wakes that leave the same mail
//! unread, and raises `needs_you`. The session's own inbox notice (its inbox waiter,
//! which wakes it as mail arrives) counts as the first of those for the
//! mail it told it about, so a wake routine never types the same news in seconds after
//! the notice. Any further loop limit of a fresh routine is its agent's to decide, in
//! its task.
//!
//! The run's `{{trigger.sessions}}` is a JSON array of `{handle, name, unread,
//! oldest_unread_at}` and `{{trigger.count}}` its length (fresh runs only: a wake
//! renders no trigger fields). A routine reaches the sessions an `agent_done` trigger
//! would: a team routine those launched in its team, a personal routine those its
//! account launched in its org. A session whose launch recorded neither fires none; for
//! a wake routine, also one launched from no saved agent, or from another agent.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::Serialize;

use crate::commands::dashboard_launch::SessionOwner;
use crate::coordinator::StoreResult;

use super::admit::{self, Admission, Fire, Outcome};
use super::discover::DiscoveredRoutine;
use super::model::{RoutineDef, RoutineKey, RoutineTrigger, EVENT_INBOX_IDLE};
use super::state::RoutineStateStore;
use super::tick::{Account, RunSource};

/// How long offers are collected before the session list is read: long enough for a
/// turn-end's state write to land, short enough that a wake feels prompt.
pub const SETTLE: Duration = Duration::from_secs(2);

/// How long to wait before looking again at an offered session that was not idle yet.
pub const RECHECK: Duration = Duration::from_secs(5);

/// How many more times an offered session that was not idle yet is looked at.
pub const RECHECKS: u8 = 2;

/// The least time between two fires of one routine.
pub const WINDOW_MS: i64 = 30_000;

/// After this long, a session whose mail is still unread is offered once more.
pub const REOFFER_MS: i64 = 10 * 60_000;

/// How many wakes a `wake` routine gives one session for the same unread mail
/// before it stops and raises `needs_you` (SW-AGR-2, §9.8). The count lives
/// with the trigger's other marks, in memory on this Mac: an app restart
/// starts it over (at most three more wakes for that mail, then `needs_you`).
pub const MAX_IGNORED_WAKES: u8 = 3;

/// A `told` mark whose `needs_you` was raised: never offered again for that mail.
const RAISED: u8 = u8::MAX;

/// How long after the app starts it sweeps for idle sessions with unread mail.
pub const BOOT_SWEEP_DELAY: Duration = Duration::from_secs(60);

/// One session idle at its prompt with unread mail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdleMail {
    pub handle: String,
    /// The name its tab shows.
    pub name: String,
    pub unread: u32,
    /// When the oldest unread message was sent, RFC 3339.
    pub oldest_unread_at: Option<String>,
    /// The newest unread message's id in its team's inbox.
    #[serde(skip)]
    pub newest_id: i64,
    /// The org and team whose inbox holds its mail.
    #[serde(skip)]
    pub inbox_org: String,
    #[serde(skip)]
    pub inbox_team: String,
    /// Who launched it and where, when its launch recorded it.
    #[serde(skip)]
    pub owner: Option<SessionOwner>,
    /// The saved agent it runs, by key (no revision), when it was launched
    /// from one: what a `wake` routine matches against its own agent.
    #[serde(skip)]
    pub agent: Option<String>,
    /// When its own inbox waiter last told it about its mail, in its team's inbox.
    #[serde(skip)]
    pub announced: Option<crate::weft::inbox::Announced>,
}

/// One routine's record of offering one session its mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Told {
    newest_id: i64,
    at: i64,
    /// How many times this mail was offered (the first offer and the re-offer).
    times: u8,
}

/// `(routine key, handle, inbox org, inbox team)`: message ids are only comparable
/// within one inbox.
type ToldKey = (String, String, String, String);

/// What the trigger keeps between offers, on this Mac.
#[derive(Debug, Default)]
pub struct InboxIdleState {
    /// Handles offered since the last check, with how many rechecks each has had.
    pending: HashMap<String, u8>,
    /// Whether a check is already scheduled to take `pending`.
    scheduled: bool,
    /// When each routine last fired from this trigger, epoch ms.
    last_fire: HashMap<String, i64>,
    told: HashMap<ToldKey, Told>,
}

impl InboxIdleState {
    /// Offer `handles` (with the recheck count each has had). True when no check is
    /// scheduled yet, so the caller must schedule one.
    pub fn note<'a>(&mut self, handles: impl IntoIterator<Item = (&'a str, u8)>) -> bool {
        for (handle, rechecks) in handles {
            let slot = self.pending.entry(handle.to_string()).or_insert(rechecks);
            *slot = (*slot).min(rechecks);
        }
        if self.scheduled || self.pending.is_empty() {
            return false;
        }
        self.scheduled = true;
        true
    }

    /// Everything offered since the last take, with its recheck count; the next offer
    /// schedules a new check.
    pub fn take(&mut self) -> Vec<(String, u8)> {
        self.scheduled = false;
        let mut handles: Vec<(String, u8)> = self.pending.drain().collect();
        handles.sort();
        handles
    }

    /// Forget every session not in `live`: its marks can never matter again.
    pub fn prune(&mut self, live: &HashSet<String>) {
        self.told.retain(|(_, h, _, _), _| live.contains(h));
    }

    fn told_key(routine: &str, s: &IdleMail) -> ToldKey {
        (
            routine.to_string(),
            s.handle.clone(),
            s.inbox_org.clone(),
            s.inbox_team.clone(),
        )
    }

    /// Whether `routine` should be offered `s` now: mail newer than it was offered, or
    /// the same mail still unread [`REOFFER_MS`] after its last offer, while it was
    /// offered fewer than `offers` times.
    fn has_news(&self, routine: &str, s: &IdleMail, now: i64, offers: u8) -> bool {
        match self.told.get(&Self::told_key(routine, s)) {
            None => true,
            Some(t) if s.newest_id > t.newest_id => true,
            Some(t) => t.times < offers && now - t.at >= REOFFER_MS,
        }
    }

    /// A `wake` routine and `s`'s own inbox notice share one offer per batch: mail the
    /// notice already told `s` about counts as the routine's first offer of it, made when
    /// the notice told it. The routine does not type the same news in again until
    /// [`REOFFER_MS`] after that, and the notice counts toward its
    /// [`MAX_IGNORED_WAKES`].
    fn count_notice(&mut self, routine: &str, s: &IdleMail) {
        let Some(notice) = s.announced.as_ref().filter(|n| n.through_id >= s.newest_id) else {
            return;
        };
        let key = Self::told_key(routine, s);
        if self
            .told
            .get(&key)
            .is_some_and(|t| t.newest_id >= s.newest_id)
        {
            return;
        }
        self.told.insert(
            key,
            Told {
                newest_id: s.newest_id,
                at: notice.at_ms,
                times: 1,
            },
        );
    }

    /// Whether a `wake` routine woke `s` [`MAX_IGNORED_WAKES`] times for the same
    /// mail and it is still unread: it stops, and raises `needs_you` once.
    fn ignored(&mut self, routine: &str, s: &IdleMail, now: i64) -> bool {
        match self.told.get_mut(&Self::told_key(routine, s)) {
            Some(t)
                if s.newest_id == t.newest_id
                    && t.times >= MAX_IGNORED_WAKES
                    && t.times != RAISED
                    && now - t.at >= REOFFER_MS =>
            {
                t.times = RAISED;
                true
            }
            _ => false,
        }
    }

    fn tell(&mut self, routine: &str, s: &IdleMail, now: i64) {
        let key = Self::told_key(routine, s);
        let times = match self.told.get(&key) {
            Some(t) if t.newest_id == s.newest_id => t.times.saturating_add(1),
            _ => 1,
        };
        self.told.insert(
            key,
            Told {
                newest_id: s.newest_id,
                at: now,
                times,
            },
        );
    }

    /// When `routine` may fire next, if not now: the end of its window.
    fn next_fire(&self, routine: &str, now: i64) -> Option<i64> {
        self.last_fire
            .get(routine)
            .filter(|last| now - **last < WINDOW_MS)
            .map(|last| last + WINDOW_MS)
    }

    fn fired(&mut self, routine: &str, now: i64) {
        self.last_fire.insert(routine.to_string(), now);
    }
}

/// What one offer did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Offered {
    /// `(routine key, admission)` of every fire.
    pub admissions: Vec<(String, Admission)>,
    /// Sessions to offer again and when (epoch ms): those a routine could not take yet,
    /// and those it was just fired for, to re-offer if their mail is still unread.
    pub rechecks: Vec<(i64, Vec<String>)>,
}

impl Offered {
    fn recheck(&mut self, at: i64, sessions: &[&IdleMail]) {
        let handles: Vec<String> = sessions.iter().map(|s| s.handle.clone()).collect();
        match self.rechecks.iter_mut().find(|(when, _)| *when == at) {
            Some((_, existing)) => {
                for h in handles {
                    if !existing.contains(&h) {
                        existing.push(h);
                    }
                }
            }
            None => self.rechecks.push((at, handles)),
        }
    }
}

/// Whether `def` has an `inbox_idle` trigger.
pub fn matches(def: &RoutineDef) -> bool {
    def.triggers
        .iter()
        .any(|t| matches!(t, RoutineTrigger::Event(spec) if spec.kind.trim() == EVENT_INBOX_IDLE))
}

/// Whether routine `key` reaches session `s`: the same rule as `agent_done`
/// (`super::event::scope`), so a session whose launch recorded no team or account fires
/// no routine.
pub fn in_reach(key: &RoutineKey, s: &IdleMail) -> bool {
    super::event::scope(key, s.owner.as_ref()) == super::event::Scope::In
}

/// Whether `s` runs the agent `agent` (a key, no revision), compared the way
/// a wake picks its session (`wake::in_target`). A session launched from no
/// saved agent, or a routine whose agent cannot be named, matches nothing.
fn is_agents(s: &IdleMail, agent: Option<&str>) -> bool {
    matches!((s.agent.as_deref(), agent), (Some(a), Some(b)) if a == b)
}

/// The run's `{{trigger.*}}` fields.
fn payload(sessions: &[&IdleMail]) -> serde_json::Value {
    serde_json::json!({
        "count": sessions.len(),
        "sessions": sessions,
    })
}

/// Fire every routine this app fires (armed here, enabled) with an `inbox_idle` trigger
/// for the `sessions` (already known idle with unread mail) in its reach that it should
/// be offered (see the module docs), each run dispatched from this Mac's `account`. What
/// a routine cannot take yet goes to [`Offered::rechecks`] and writes no run row. Each
/// admission is its own transaction, so one routine's failure does not lose another's.
pub fn offer(
    account: &Account,
    routines: &[DiscoveredRoutine],
    store: &RoutineStateStore,
    state: &mut InboxIdleState,
    sessions: &[IdleMail],
    now: i64,
) -> StoreResult<Offered> {
    let mut offered = Offered::default();
    if sessions.is_empty() {
        return Ok(offered);
    }
    let host =
        super::host::local_host_id(&account.0).map_err(crate::coordinator::StoreError::Io)?;
    for routine in routines {
        if !routine.armed_here || routine.held() || !matches(&routine.def) {
            continue;
        }
        match store.is_enabled(&routine.key, &routine.def, &host) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => {
                log::warn!(target: "routines", "routine {}: state unreadable: {e}", routine.key);
                continue;
            }
        }
        let key = routine.key.to_string();
        // A wake routine types into the session itself: it tries up to
        // MAX_IGNORED_WAKES times for the same mail, then asks the human.
        let wake =
            super::service::effective_mode(&account.0, &account.1, &routine.key, &routine.def)
                == super::model::RoutineMode::Wake;
        let offers = if wake { MAX_IGNORED_WAKES } else { 2 };
        // A wake routine types into its own agent's session, so only that
        // agent's mail fires it; a fresh routine stays team-wide.
        let own_agent = wake
            .then(|| routine.def.agent.key_for(&routine.key).ok())
            .flatten()
            .map(|key| key.to_string());
        let mut reached = Vec::new();
        let mut ignored = Vec::new();
        for s in sessions {
            if !in_reach(&routine.key, s) || !uses_scratch(account, &routine.key, s) {
                continue;
            }
            if wake && !is_agents(s, own_agent.as_deref()) {
                continue;
            }
            if wake {
                state.count_notice(&key, s);
            }
            if wake && state.ignored(&key, s, now) {
                ignored.push(s);
            } else if state.has_news(&key, s, now, offers) {
                reached.push(s);
            }
        }
        if !ignored.is_empty() {
            raise_ignored(store, routine, &ignored, now);
        }
        if reached.is_empty() {
            continue;
        }
        if !state.last_fire.contains_key(&key) {
            if let Some(at) = last_fire_in_log(store, &routine.key)? {
                state.last_fire.insert(key.clone(), at);
            }
        }
        if let Some(at) = state.next_fire(&key, now) {
            offered.recheck(at, &reached);
            continue;
        }
        let occurrence_key = format!("{key}@inbox_idle:{now}");
        let fire = Fire {
            key: &routine.key,
            principal: &account.1,
            def: &routine.def,
            source: RunSource::Event,
            occurrence_key: Some(&occurrence_key),
            scheduled_at: None,
            now,
            variables: Default::default(),
            trigger_payload: Some(payload(&reached)),
            cause: None,
        };
        // A run still going may have looked before this mail arrived: wait for it
        // to end rather than folding into it, and write no row meanwhile.
        let admitted = store.transaction(|tx| {
            let active = if wake {
                super::runs::active_wake_run(tx, &key)?
            } else {
                super::runs::active_run(tx, &key)?
            };
            if active.is_some() {
                return Ok(None);
            }
            admit::admit(tx, &account.0, &fire).map(Some)
        });
        let admission = match admitted {
            Ok(Some(admission)) => admission,
            Ok(None) => {
                offered.recheck(now + WINDOW_MS, &reached);
                continue;
            }
            Err(e) => {
                log::warn!(target: "routines", "routine {key}: inbox_idle fire failed: {e}");
                continue;
            }
        };
        match &admission.outcome {
            // A run will see these sessions: they are told, and offered once more
            // later if their mail is still unread then.
            Outcome::Queued | Outcome::Held => {
                state.fired(&key, now);
                for s in &reached {
                    state.tell(&key, s, now);
                }
                offered.recheck(now + REOFFER_MS, &reached);
            }
            Outcome::Coalesced { .. } => {
                state.fired(&key, now);
                offered.recheck(now + WINDOW_MS, &reached);
            }
            // Nowhere to run, or refused by policy: one row per window at most, and
            // the next mail offers them again.
            Outcome::Skipped { .. } => state.fired(&key, now),
            Outcome::Duplicate => {}
        }
        offered.admissions.push((key, admission));
    }
    Ok(offered)
}

/// A `wake` routine's sessions that left the same mail unread through
/// [`MAX_IGNORED_WAKES`] wakes: one `needs_you` run row says so, and nothing
/// is typed into them again for that mail.
fn raise_ignored(
    store: &RoutineStateStore,
    routine: &DiscoveredRoutine,
    sessions: &[&IdleMail],
    now: i64,
) {
    let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
    let detail = format!(
        "{} left the same unread mail through {MAX_IGNORED_WAKES} wakes; not waking again until new mail arrives",
        names.join(", ")
    );
    let key = routine.key.to_string();
    let occurrence_key = format!("{key}@inbox_idle:{now}:ignored");
    let written = store.transaction(|tx| {
        super::runs::record_needs_you(
            tx,
            &key,
            routine.def.revision,
            &occurrence_key,
            now,
            &detail,
        )
    });
    match written {
        Ok(run_id) => super::events::changed(&key, &run_id),
        Err(e) => log::warn!(target: "routines", "routine {key}: needs_you not recorded: {e}"),
    }
}

/// When `key` last fired from this trigger, by its run log: a restart keeps the window.
fn last_fire_in_log(store: &RoutineStateStore, key: &RoutineKey) -> StoreResult<Option<i64>> {
    let prefix = format!("{key}@inbox_idle:");
    Ok(store
        .runs(key)?
        .into_iter()
        .filter(|r| {
            r.occurrence_key
                .as_deref()
                .is_some_and(|k| k.starts_with(&prefix))
        })
        .map(|r| r.created_at)
        .max())
}

/// A personal routine never acts on a session of a team that has "Use my scratch items"
/// off, as for `agent_done`.
fn uses_scratch(account: &Account, key: &RoutineKey, s: &IdleMail) -> bool {
    if key.account.is_none() {
        return true;
    }
    match s.owner.as_ref().and_then(|o| o.team.as_deref()) {
        Some(team) => crate::context_repo::scratch_space::team_uses_scratch(
            &account.0, &account.1, &key.org, team,
        ),
        None => true,
    }
}

/// A message was stored for `handle`: check it for the app's `inbox_idle` routines after
/// [`SETTLE`]. Nothing is typed into it.
pub fn note(app: &tauri::AppHandle, handle: &str) {
    note_all(app, [(handle, 0)]);
}

fn note_all<'a>(app: &tauri::AppHandle, handles: impl IntoIterator<Item = (&'a str, u8)>) {
    use tauri::Manager;
    let Some(ticker) = app.try_state::<super::tick::RoutineTicker>() else {
        return;
    };
    if ticker.note_inbox_idle(handles) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(SETTLE).await;
            check(&app).await;
        });
    }
}

/// Offer `handles` again after `delay`. A later offer never waits behind it: it only
/// adds these to whatever check is pending then.
fn note_later(app: &tauri::AppHandle, handles: Vec<(String, u8)>, delay: Duration) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        note_all(&app, handles.iter().map(|(h, n)| (h.as_str(), *n)));
    });
}

/// After [`BOOT_SWEEP_DELAY`], offer every session with unread mail once: mail that
/// arrived while the app was closed has no other offer.
pub fn sweep_at_boot(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(BOOT_SWEEP_DELAY).await;
        match crate::mcp::session_list::read_session_list_and_unread(&app, None).await {
            Ok((_, unread)) => {
                note_all(&app, unread.found.keys().map(|h| (h.as_str(), 0)));
            }
            Err(e) => log::warn!(target: "routines", "inbox_idle boot sweep: {e}"),
        }
    });
}

/// Take every offered session, keep those the live session list shows idle with unread
/// mail, and offer them to the routines. Nothing is read when no routine has the trigger.
async fn check(app: &tauri::AppHandle) {
    use crate::commands::dashboard_launch::DashboardLaunchState;
    use tauri::Manager;
    let Some(ticker) = app.try_state::<super::tick::RoutineTicker>() else {
        return;
    };
    let offered = ticker.take_inbox_idle();
    if offered.is_empty() {
        return;
    }
    let probe = app.clone();
    let wanted = crate::commands::await_blocking("routine_inbox_idle_wanted", move || {
        Ok(probe
            .try_state::<super::tick::RoutineTicker>()
            .is_some_and(|t| t.wants_inbox_idle()))
    })
    .await
    .unwrap_or(false);
    if !wanted {
        return;
    }
    let (rows, unread) =
        match crate::mcp::session_list::read_session_list_and_unread(app, None).await {
            Ok(read) => read,
            Err(e) => {
                log::warn!(target: "routines", "inbox_idle: session list unreadable: {e}");
                return;
            }
        };
    ticker.prune_inbox_idle(&rows.iter().map(|r| r.handle.clone()).collect());
    let launch = app.try_state::<DashboardLaunchState>();
    let found = idle_with_mail(&offered, rows, &unread.found, |handle| {
        let launch = launch.as_ref()?;
        let session = launch.resolve(handle)?;
        launch.owner_for_session(&session)
    });
    if !found.not_yet.is_empty() {
        note_later(app, found.not_yet, RECHECK);
    }
    if found.idle.is_empty() {
        return;
    }
    let sessions = found.idle;
    let app2 = app.clone();
    let result = crate::commands::await_blocking("routine_offer_inbox_idle", move || {
        Ok(app2
            .try_state::<super::tick::RoutineTicker>()
            .and_then(|t| t.offer_inbox_idle(&sessions)))
    })
    .await;
    match result {
        Ok(Some(offered)) => {
            let now = chrono::Utc::now().timestamp_millis();
            for (at, handles) in offered.rechecks {
                let delay = Duration::from_millis((at - now).max(0) as u64);
                note_later(app, handles.into_iter().map(|h| (h, 0)).collect(), delay);
            }
        }
        Ok(None) => {}
        Err(e) => log::warn!(target: "routines", "inbox_idle offer failed: {e}"),
    }
}

/// What [`idle_with_mail`] found among the offered sessions.
#[derive(Debug, Default)]
pub(crate) struct Found {
    /// Idle after a clean turn, with unread mail.
    pub idle: Vec<IdleMail>,
    /// Alive with unread mail but not idle yet, to look at again (with the recheck
    /// count each will have had).
    pub not_yet: Vec<(String, u8)>,
}

/// Sort the `offered` handles (with their recheck counts) by what `rows` (the live
/// session list) and `unread` say now; `owner_of` says who launched each.
pub(crate) fn idle_with_mail(
    offered: &[(String, u8)],
    rows: Vec<crate::mcp::session_list::SessionListRow>,
    unread: &HashMap<String, crate::commands::agent_inbox::SessionUnread>,
    owner_of: impl Fn(&str) -> Option<SessionOwner>,
) -> Found {
    use crate::mcp::session_list::SessionState;
    let mut found = Found::default();
    for row in rows {
        let Some((_, rechecks)) = offered.iter().find(|(h, _)| *h == row.handle) else {
            continue;
        };
        let Some(mail) = unread.get(&row.handle).filter(|m| m.count > 0) else {
            continue;
        };
        let Some(newest_id) = mail.newest_id else {
            continue;
        };
        match row.state {
            SessionState::Idle if !row.failed => found.idle.push(IdleMail {
                owner: owner_of(&row.handle),
                agent: row.agent,
                name: row.display_name,
                unread: mail.count,
                oldest_unread_at: row.oldest_unread_at,
                newest_id,
                inbox_org: mail.team.org.clone(),
                inbox_team: mail.team.team.clone(),
                // What this session was told: a mark another session under the handle
                // left is not its notice.
                announced: mail
                    .announced
                    .clone()
                    .filter(|told| row.session.as_deref() == Some(told.session.as_str())),
                handle: row.handle,
            }),
            SessionState::Running | SessionState::NeedsYou | SessionState::Unknown
                if *rechecks < RECHECKS =>
            {
                found.not_yet.push((row.handle, rechecks + 1));
            }
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_routine_waits_out_its_window() {
        let mut state = InboxIdleState::default();
        let r = "acme/api/backend/wake";
        assert_eq!(state.next_fire(r, 0), None);
        state.fired(r, 0);
        assert_eq!(state.next_fire(r, 1_000), Some(WINDOW_MS));
        assert_eq!(state.next_fire(r, WINDOW_MS), None);
    }
}
