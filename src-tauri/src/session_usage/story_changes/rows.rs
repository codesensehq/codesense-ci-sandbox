// SPDX-License-Identifier: MIT
//! The story kind of the contribution store: one `session` row per session of a story, folded
//! into the session half of the story's totals. A row holds what the session adds (cost, time,
//! the span of its usage, where it ran) and nothing the rest of the story could change, so one
//! usage, turn or end event rewrites one row and never the story's other sessions.
use super::*;
use std::{cell::RefCell, path::PathBuf};
use crate::session_usage::contribution::{self, Contribution, EntityAdapter, Of, Op, Reducer, Row, Source, SourceResolver};
use crate::session_usage::{cost_rollup::CostRollup, SessionUsage};

pub(super) const SESSION: &str = "session";

pub(super) const COST_UNKNOWN: &str = "one or more sessions have no complete priced usage";
const TIME_UNKNOWN: &str = "one or more sessions have no observed timing; time covers observed sessions only";
const COST_NONE: &str = "no sessions with priced usage for this story";
/// No sessions while managed Claude settings send their usage elsewhere.
const COST_REDIRECTED: &str = crate::session_usage::managed_telemetry::USAGE_REDIRECTED;

/// A stored `costUsd` reason as it reads now. A kept row says whatever held when it was folded,
/// so a read swaps a no-sessions reason for the one that holds now; any other reason stays.
pub(crate) fn current_cost_reason(reason: &str, redirected: bool) -> &str {
    if reason == COST_NONE || reason == COST_REDIRECTED {
        no_sessions_cost(redirected)
    } else {
        reason
    }
}

fn no_sessions_cost(redirected: bool) -> &'static str {
    if redirected {
        COST_REDIRECTED
    } else {
        COST_NONE
    }
}

/// The story kind's aggregate: what its session rows fold to.
const REDUCERS: &[Reducer] = &[
    Reducer { out: "sessions", op: Op::Count, of: Of::Row, source: SESSION },
    Reducer { out: "cost", op: Op::Sum, of: Of::Num("cost"), source: SESSION },
    Reducer { out: "walls", op: Op::Sum, of: Of::Num("wall"), source: SESSION },
    Reducer { out: "active", op: Op::Sum, of: Of::Num("active"), source: SESSION },
    Reducer { out: "idle", op: Op::Sum, of: Of::Num("idle"), source: SESSION },
    Reducer { out: "timed", op: Op::Count, of: Of::Num("timed"), source: SESSION },
    Reducer { out: "untimed", op: Op::Count, of: Of::Num("untimed"), source: SESSION },
    Reducer { out: "start", op: Op::Min, of: Of::Num("start"), source: SESSION },
    Reducer { out: "end", op: Op::Max, of: Of::Num("end"), source: SESSION },
    Reducer { out: "first", op: Op::Min, of: Of::First, source: SESSION },
    Reducer { out: "last", op: Op::Max, of: Of::Last, source: SESSION },
    Reducer { out: "runtimes", op: Op::Union, of: Of::Extra("runtime"), source: SESSION },
];

/// The story kind.
pub(crate) struct StoryRows {
    pub home: PathBuf,
    /// What a sweep of sources shares, built on the first source that needs it.
    shared: RefCell<Option<Shared>>,
}

/// The story resolver and the stored story links every source of a sweep reads, made once.
struct Shared {
    resolver: StoryResolver,
    stored: Option<crate::session_usage::StoredStories>,
}

impl StoryRows {
    pub(crate) fn new(home: PathBuf) -> Self {
        Self { home, shared: Default::default() }
    }
}

/// How many resolvers the adapters for `home` have built (tests only).
#[cfg(test)]
static RESOLVER_BUILDS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn resolver_builds(home: &std::path::Path) -> usize {
    let builds = RESOLVER_BUILDS.lock().unwrap();
    builds.iter().filter(|built| built.as_path() == home).count()
}

impl EntityAdapter for StoryRows {
    const KIND: &'static str = "story";
    /// Bump it with any change to what a session row holds.
    const VERSION: i64 = 2;
    const REDUCERS: &'static [Reducer] = REDUCERS;
    const SOURCES: &'static [&'static str] = &[SESSION];
}

impl SourceResolver for StoryRows {
    fn contributions(
        &self,
        store: &UsageStore,
        queued: &Source,
    ) -> Result<(Source, Vec<(String, Contribution)>), String> {
        let identity = session_read::resolve_session(store, None, &queued.id)?;
        let source = Source { kind: SESSION, id: identity.session_id };
        let mut guard = self.shared.borrow_mut();
        let shared = Option::get_or_insert_with(&mut *guard, || {
            #[cfg(test)]
            RESOLVER_BUILDS.lock().unwrap().push(self.home.clone());
            Shared { resolver: StoryResolver::for_home(&self.home, Default::default()), stored: None }
        });
        let report = store.report_with_stored(
            &Filter {
                session_id: Some(queued.id.clone()),
                ..Default::default()
            },
            &mut shared.resolver,
            &mut shared.stored,
        )?;
        let mut by_story: BTreeMap<String, Vec<&SessionUsage>> = Default::default();
        for usage in &report.sessions {
            if let Some(story) = &usage.story {
                by_story.entry(story.id.to_lowercase()).or_default().push(usage);
            }
        }
        let mut rows = vec![];
        for (story, usages) in by_story {
            rows.push((story, session_row(store, &source.id, &usages)?));
        }
        Ok((source, rows))
    }

    fn rebuild(
        &self,
        store: &UsageStore,
        scope: &CallerScope,
        entity: &str,
    ) -> Result<Vec<(Source, Contribution)>, String> {
        rows_from_sessions(
            store,
            &story_report(store, &self.home, entity, scope)?.sessions,
        )
    }
}

/// A story's session rows from its sessions in a usage report (which `story_changes` already
/// has, so it is not made twice).
pub(super) fn rows_from_sessions(
    store: &UsageStore,
    usages: &[SessionUsage],
) -> Result<Vec<(Source, Contribution)>, String> {
    let mut by_session: BTreeMap<String, Vec<&SessionUsage>> = Default::default();
    for usage in usages {
        let id = session_read::resolve_session(store, None, &usage.session_id)?.session_id;
        by_session.entry(id).or_default().push(usage);
    }
    let mut rows = vec![];
    for (id, usages) in by_session {
        let row = session_row(store, &id, &usages)?;
        rows.push((Source { kind: SESSION, id }, row));
    }
    Ok(rows)
}

/// One session's row for a story: its cost (the one calculation, null when any model is
/// unpriced), its observed timing, the span of its usage and where it ran. `usages` are the
/// report's entries for it.
pub(super) fn session_row(store: &UsageStore, session: &str, usages: &[&SessionUsage]) -> Result<Contribution, String> {
    let record = store.read_session_record(session)?;
    let timing = record.as_ref().and_then(|r| r.timing.clone());
    let mut row = Contribution::new();
    row.num("cost", CostRollup::total(&store.session_cost(session)?), COST_UNKNOWN);
    let used: Vec<&&SessionUsage> = usages.iter().filter(|u| u.messages > 0).collect();
    let usage_first = used.iter().map(|u| u.first_at_ms).min();
    let usage_last = used.iter().map(|u| u.last_at_ms).max();
    let mut recent = usage_last;
    match &timing {
        Some(t) => {
            let end = t.started_at_ms + t.wall_ms;
            row.num("wall", Some(t.wall_ms as f64), "");
            row.num("start", Some(t.started_at_ms as f64), "");
            row.num("end", Some(end as f64), "");
            row.num("active", Some(t.active_ms as f64), "");
            row.num("idle", Some(t.idle_ms as f64), "");
            row.num("timed", Some(1.0), "");
            row.num("ended", Some(if Option::is_some(&t.ended_at_ms) { 1.0 } else { 0.0 }), "");
            recent = Some(Option::map_or(recent, end, |v| Ord::max(v, end)));
        }
        None => row.num("untimed", Some(1.0), ""),
    }
    row.num("recent", recent.map(|v| v as f64), "");
    // Activity is the usage the report rolls up; the timing counts only where there is none.
    row.first_ms = usage_first.or(timing.as_ref().map(|t| t.started_at_ms));
    row.last_ms = usage_last.or(timing.as_ref().map(|t| t.started_at_ms + t.wall_ms));
    if let Some(usage) = usages.first() {
        row.extra.insert("runtime".into(), json!(codesense_pty_core::agent_cli::AgentCli::as_str(usage.cli)));
    }
    // How the session is tied to the story, as `read_sessions` gives it (its first usage's).
    let basis = Option::and_then(<[&SessionUsage]>::first(usages), |usage| {
        Option::and_then(Option::as_ref(&usage.story), |story| story.basis)
    });
    row.extra
        .insert("basis".into(), json!(Option::unwrap_or(basis, "exact")));
    for (name, value) in [
        ("branch", record.as_ref().and_then(|r| r.branch.clone())),
        ("worktree", record.as_ref().and_then(|r| r.worktree.clone())),
        ("workspace", record.as_ref().and_then(|r| r.workspace.clone())),
    ] {
        row.extra.insert(name.into(), json!(value));
    }
    Ok(row)
}

/// The session half of a story's totals, from its rows.
pub(super) struct SessionFold {
    pub count: usize,
    pub cost: f64,
    pub wall: Option<i64>,
    pub timed: bool,
    pub time: TimeFacts,
    pub runtimes: BTreeSet<String>,
    pub unavailable: BTreeMap<&'static str, &'static str>,
    pub activity: Activity,
    /// Sessions with timing and no end whose last activity is within `OPEN_WINDOW_MS` of `now`.
    pub open: usize,
    /// What the git half reads from the sessions (their branches, worktrees and workspaces).
    pub git_key: String,
    /// The same, one per session, for the git half to read (no `session_read` per session).
    pub refs: Vec<super::git::SessionRef>,
}

/// `redirected`: managed Claude settings send session usage elsewhere, so no sessions says that.
pub(super) fn fold(rows: &[Row], now: i64, redirected: bool) -> SessionFold {
    let agg = contribution::aggregate(REDUCERS, rows);
    let num = |name: &str| Option::flatten(agg.nums.get(name).copied());
    let count = num("sessions").unwrap_or(0.0) as usize;
    let mut unavailable: BTreeMap<&'static str, &'static str> = Default::default();
    // No sessions is no priced usage, never a cost of 0; a session with an unpriced model is unknown.
    if count == 0 {
        unavailable.insert("costUsd", no_sessions_cost(redirected));
    } else if Option::is_none(&num("cost")) {
        unavailable.insert("costUsd", COST_UNKNOWN);
    }
    if num("untimed").unwrap_or(0.0) > 0.0 {
        unavailable.insert("time", TIME_UNKNOWN);
    }
    let timed_sessions = num("timed").unwrap_or(0.0) as usize;
    let time = TimeFacts {
        wall_ms: num("walls").unwrap_or(0.0) as i64,
        active_ms: num("active").unwrap_or(0.0) as i64,
        idle_ms: num("idle").unwrap_or(0.0) as i64,
        sessions: timed_sessions,
    };
    let mut activity = Activity { first: None, last: None };
    activity.see(num("first").map(|v| v as i64));
    activity.see(num("last").map(|v| v as i64));
    let open = rows
        .iter()
        .filter(|row| {
            let nums = &row.contribution.nums;
            matches!(nums.get("ended"), Some(Some(v)) if *v == 0.0)
                && matches!(nums.get("recent"), Some(Some(at)) if now - (*at as i64) <= OPEN_WINDOW_MS)
        })
        .count();
    let links: BTreeSet<(Option<String>, Option<String>, Option<String>)> = rows
        .iter()
        .map(|row| {
            let read = |name: &str| {
                Option::map(Option::and_then(row.contribution.extra.get(name), Value::as_str), str::to_owned)
            };
            (read("branch"), read("worktree"), read("workspace"))
        })
        .collect();
    SessionFold {
        count,
        cost: num("cost").unwrap_or(0.0),
        wall: num("start").zip(num("end")).map(|(from, to)| (to - from) as i64),
        timed: timed_sessions > 0 || count == 0,
        time,
        runtimes: Option::unwrap_or_default(agg.sets.get("runtimes").cloned()),
        unavailable,
        activity,
        open,
        git_key: serde_json::to_string(&links).unwrap_or_default(),
        refs: Iterator::collect(Iterator::map(
            <[Row]>::iter(rows),
            super::git::SessionRef::of_row,
        )),
    }
}
