// SPDX-License-Identifier: MIT
//! The routine scheduler pass: one wake of the routine scheduler thread
//! (`super::scheduler`).
//!
//! Each wake discovers the routines this app fires (armed here, enabled) across
//! every team, seeds a cursor (`routine_triggers.next_run_at`) for each schedule
//! trigger, and claims every cursor that is due. A claim is a compare-and-swap
//! on the cursor in the same SQLite transaction as the `routine_runs` rows it
//! produces, and every row carries a unique occurrence key, so two racing
//! wakes, or a crash mid-claim, can neither lose nor duplicate an occurrence.
//!
//! The pass runs on the scheduler's OS thread, so its blocking file and SQLite
//! IO never touches the async runtime.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Transaction};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, Runtime};

use crate::coordinator::StoreResult;
use crate::domain::Principal;

use super::admit::{self, Admission, Fire, Outcome};
use super::discover::{discover_routines, discovery_generation, DiscoveredRoutine};
use super::finish::{self, Sessions};
use super::model::{MissedPolicy, RoutineKey, RoutineTrigger};
use super::schedule::{self, CompiledSchedule};
use super::service::RoutineError;
use super::state::RoutineStateStore;

/// The current time as Unix epoch milliseconds. Injected so tests can drive
/// DST, downtime and late wakes deterministically.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Who is signed in and where their CodeSense home is, or `None` when nobody
/// is (then nothing fires).
pub type Ambient = Arc<dyn Fn() -> Option<Account> + Send + Sync>;

/// A CodeSense home and the principal signed in to it.
pub type Account = (PathBuf, Principal);

/// How long a discovery is reused before the next wake rescans. A teammate's
/// synced edit is therefore picked up within this window; local edits drop the
/// cache at once.
pub const DISCOVERY_TTL_MS: i64 = 60_000;

/// How long a wake waits for the signed-in account to resolve.
const AMBIENT_WAIT: Duration = Duration::from_secs(10);

pub fn system_clock() -> Clock {
    Arc::new(|| Utc::now().timestamp_millis())
}

/// Why a run row exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunSource {
    /// An occurrence that came due while the app was running, on time or late.
    Schedule,
    /// The one run a `missed: run_once` routine gets after missed occurrences.
    CatchUp,
    /// Started by hand (`run_now`).
    Manual,
    /// Fired by an event trigger (an agent finishing).
    Event,
}

impl RunSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::CatchUp => "catch_up",
            Self::Manual => "manual",
            Self::Event => "event",
        }
    }
}

/// One occurrence handed to admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub routine_key: RoutineKey,
    pub revision: u32,
    pub source: RunSource,
    /// `<routine_key>@<scheduled instant, RFC 3339 UTC>`; unique per run row.
    pub occurrence_key: String,
    /// Unix epoch milliseconds the occurrence was scheduled for.
    pub scheduled_at: i64,
    /// Unix epoch milliseconds of the wake that claimed it.
    pub claimed_at: i64,
}

/// Turns a claimed occurrence into a run, dispatched from this Mac's
/// `account`. Called inside the claim's transaction: whatever it writes commits
/// or rolls back with the cursor advance, so it must not open a transaction of
/// its own.
pub trait Admit: Send + Sync {
    fn admit(
        &self,
        tx: &Transaction<'_>,
        account: &Account,
        routine: &DiscoveredRoutine,
        occurrence: &Occurrence,
    ) -> StoreResult<Admission>;
}

/// Production admission: the routine's policy decides, through [`admit::admit`].
pub struct RoutineAdmitter;

impl Admit for RoutineAdmitter {
    fn admit(
        &self,
        tx: &Transaction<'_>,
        account: &Account,
        routine: &DiscoveredRoutine,
        occurrence: &Occurrence,
    ) -> StoreResult<Admission> {
        admit::admit(
            tx,
            &account.0,
            &Fire {
                key: &occurrence.routine_key,
                principal: &account.1,
                def: &routine.def,
                source: occurrence.source,
                occurrence_key: Some(&occurrence.occurrence_key),
                scheduled_at: Some(occurrence.scheduled_at),
                now: occurrence.claimed_at,
                variables: Default::default(),
                trigger_payload: None,
                cause: None,
            },
        )
    }
}

/// What one wake did, for logs and tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub seeded: usize,
    /// Runs queued for the claim loop (coalesced, held and skipped fires are
    /// in `changed` but not counted here).
    pub admitted: usize,
    /// Missed occurrences recorded as skipped.
    pub skipped: usize,
    /// `(routine_key, run_id)` of every run row this wake wrote, committed.
    pub changed: Vec<(String, String)>,
}

struct CachedDiscovery {
    home: PathBuf,
    principal: Principal,
    generation: u64,
    fetched_at: i64,
    routines: Arc<Vec<DiscoveredRoutine>>,
}

/// The scheduler pass's state, held by the app across wakes.
pub struct RoutineTicker {
    clock: Clock,
    admitter: Arc<dyn Admit>,
    ambient: Ambient,
    run_sessions: Arc<dyn Sessions>,
    /// Running runs whose session's liveness could not be learned at startup;
    /// re-asked every wake until a host answers.
    unknown_liveness: Mutex<HashSet<String>>,
    /// Running runs from before this launch whose session survived it; the
    /// exit sweep covers them like runs started since.
    survivors: Mutex<HashSet<String>>,
    /// When this ticker was created, at app launch. An occurrence due before
    /// it fell while the app was closed; one due after it is only ever late.
    started_at: i64,
    cache: Mutex<Option<CachedDiscovery>>,
    /// The last account resolution and when it was made. Resolving goes
    /// through the auth manager, which can wait seconds on the keychain, so
    /// it is redone at most once per [`DISCOVERY_TTL_MS`] rather than every
    /// wake of the scheduler thread.
    account: Mutex<Option<(i64, Option<Account>)>>,
    store: Mutex<Option<(PathBuf, RoutineStateStore)>>,
    /// The `inbox_idle` trigger's offers, windows and marks.
    inbox_idle: Mutex<super::inbox_idle::InboxIdleState>,
}

impl RoutineTicker {
    pub fn new(
        clock: Clock,
        admitter: Arc<dyn Admit>,
        ambient: Ambient,
        sessions: Arc<dyn Sessions>,
    ) -> Self {
        Self {
            started_at: clock(),
            clock,
            admitter,
            ambient,
            run_sessions: sessions,
            unknown_liveness: Mutex::new(HashSet::new()),
            survivors: Mutex::new(HashSet::new()),
            cache: Mutex::new(None),
            account: Mutex::new(None),
            store: Mutex::new(None),
            inbox_idle: Mutex::new(Default::default()),
        }
    }

    /// The production ticker: wall clock, policy admission, the signed-in
    /// account (or the local guest) resolved through the app, and the app's
    /// sessions for the watchdog and liveness re-checks.
    pub fn for_app(app: AppHandle) -> Self {
        let sessions = Arc::new(finish::AppSessions::new(app.clone()));
        Self::new(
            system_clock(),
            Arc::new(RoutineAdmitter),
            Arc::new(move || ambient_blocking(&app)),
            sessions,
        )
    }

    /// Re-ask the liveness of these running runs' sessions on every wake until
    /// a host answers.
    pub fn recheck_liveness(&self, run_ids: impl IntoIterator<Item = String>) {
        self.unknown_liveness
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(run_ids);
    }

    /// Sweep these running runs from before this launch, whose sessions
    /// survived it, like runs started since.
    pub fn watch_survivors(&self, run_ids: impl IntoIterator<Item = String>) {
        self.survivors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(run_ids);
    }

    /// One wake. Returns `None` when nothing could be evaluated (signed out, or
    /// discovery or the store failed); every failure is logged and retried next
    /// wake with the cursors untouched.
    pub fn tick(&self) -> Option<TickReport> {
        let now = (self.clock)();
        let started_at = self.started_at;
        let (home, principal) = self.account(now)?;
        let store = match self.store(&home) {
            Ok(s) => s,
            Err(e) => {
                log::warn!(target: "routines", "routine store unavailable: {e}");
                return None;
            }
        };
        // Runs already started are bounded and settled whatever discovery
        // finds: they carry their own limits.
        self.maintain(now, &store);
        let routines = match self.discovered(&home, &principal, now) {
            Ok(r) => r,
            Err(e) => {
                log::warn!(target: "routines", "routine discovery failed: {e}");
                return None;
            }
        };
        let account = (home, principal);
        match run_routine_tick_with(
            now,
            started_at,
            &account,
            &routines,
            &store,
            self.admitter.as_ref(),
        ) {
            Ok(report) => {
                announce(&report.changed);
                if report.admitted > 0 || report.skipped > 0 {
                    log::info!(
                        target: "routines",
                        "routine tick: {} admitted, {} missed occurrences skipped",
                        report.admitted,
                        report.skipped
                    );
                }
                Some(report)
            }
            Err(e) => {
                log::warn!(target: "routines", "routine tick failed: {e}");
                None
            }
        }
    }

    /// Offer a finished agent to every event routine this app fires, through
    /// the same cached account, discovery and store as the scheduler pass.
    /// Returns what each matching routine's admission decided; `None` when
    /// nothing could be evaluated (signed out, or discovery or the store
    /// failed), which is logged.
    pub fn offer_agent_done(
        &self,
        event: &super::event::AgentDoneEvent,
    ) -> Option<Vec<(String, Admission)>> {
        let now = (self.clock)();
        let (home, principal) = self.account(now)?;
        let routines = match self.discovered(&home, &principal, now) {
            Ok(r) => r,
            Err(e) => {
                log::warn!(target: "routines", "routine discovery failed: {e}");
                return None;
            }
        };
        let store = match self.store(&home) {
            Ok(s) => s,
            Err(e) => {
                log::warn!(target: "routines", "routine store unavailable: {e}");
                return None;
            }
        };
        match super::event::offer(&(home, principal), &routines, &store, event, now) {
            Ok(admissions) => {
                announce_admissions(&admissions);
                Some(admissions)
            }
            Err(e) => {
                log::warn!(
                    target: "routines",
                    "routine offer of session {} failed: {e}",
                    event.session_id
                );
                None
            }
        }
    }

    pub fn offer_metadata(
        &self,
        index: &crate::context_query::service::ContextIndexService,
        change: &crate::context_query::service::SnapshotChange,
        viewer: &crate::context_query::engine::Viewer,
    ) -> Option<Vec<(String, Admission)>> {
        let now = (self.clock)();
        // Index publication follows profile switches immediately; the scheduler's
        // account cache may still name the previous profile for up to a minute.
        let (home, principal) = (self.ambient)()?;
        if !index.is_current(change) || change.owner != principal.numeric_id().ok() {
            return Some(vec![]);
        }
        let offered: Result<_, RoutineError> = (|| {
            let routines = self.discovered(&home, &principal, now)?;
            let store = self.store(&home)?;
            if !index.is_current(change) {
                return Ok(vec![]);
            }
            super::metadata::offer(&(home, principal), &routines, &store, change, viewer, now)
                .map_err(RoutineError::from)
        })();
        match offered {
            Ok(admissions) => {
                announce_admissions(&admissions);
                Some(admissions)
            }
            Err(e) => {
                log::warn!(target: "routines", "metadata offer failed: {e}");
                None
            }
        }
    }

    /// Offer a message queued for a role to every `role_pending` routine this app fires,
    /// through the same cached account, discovery and store as the scheduler pass.
    pub fn offer_role_pending(
        &self,
        event: &super::role_pending::RolePendingEvent,
    ) -> Option<Vec<(String, Admission)>> {
        let now = (self.clock)();
        let (home, principal) = self.account(now)?;
        let routines = match self.discovered(&home, &principal, now) {
            Ok(r) => r,
            Err(e) => {
                log::warn!(target: "routines", "routine discovery failed: {e}");
                return None;
            }
        };
        if !routines
            .iter()
            .any(|r| r.armed_here && super::role_pending::matches(&r.def))
        {
            return Some(Vec::new());
        }
        let store = match self.store(&home) {
            Ok(s) => s,
            Err(e) => {
                log::warn!(target: "routines", "routine store unavailable: {e}");
                return None;
            }
        };
        match super::role_pending::offer(&(home, principal), &routines, &store, event, now) {
            Ok(admissions) => {
                announce_admissions(&admissions);
                Some(admissions)
            }
            Err(e) => {
                log::warn!(
                    target: "routines",
                    "routine role_pending offer of message {} failed: {e}",
                    event.message_id
                );
                None
            }
        }
    }

    /// Offer `handles` (with the rechecks each has had) to the `inbox_idle` trigger. True
    /// when no check is scheduled yet, so the caller must schedule one.
    pub fn note_inbox_idle<'a>(&self, handles: impl IntoIterator<Item = (&'a str, u8)>) -> bool {
        self.inbox_idle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .note(handles)
    }

    /// Every handle offered to the `inbox_idle` trigger since the last take.
    pub fn take_inbox_idle(&self) -> Vec<(String, u8)> {
        self.inbox_idle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Drop the `inbox_idle` marks of every session not in `live`.
    pub fn prune_inbox_idle(&self, live: &std::collections::HashSet<String>) {
        self.inbox_idle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .prune(live);
    }

    /// Whether any routine this app fires (armed here) has an `inbox_idle` trigger, so
    /// an offer is worth reading the session list for. Blocking: it may rescan.
    pub fn wants_inbox_idle(&self) -> bool {
        let now = (self.clock)();
        let Some((home, principal)) = self.account(now) else {
            return false;
        };
        match self.discovered(&home, &principal, now) {
            Ok(routines) => routines
                .iter()
                .any(|r| r.armed_here && super::inbox_idle::matches(&r.def)),
            Err(e) => {
                log::warn!(target: "routines", "routine discovery failed: {e}");
                false
            }
        }
    }

    /// Offer sessions idle with unread mail to every `inbox_idle` routine this app
    /// fires, through the same cached account, discovery and store as the scheduler
    /// pass. `None` when nothing could be evaluated, which is logged.
    pub fn offer_inbox_idle(
        &self,
        sessions: &[super::inbox_idle::IdleMail],
    ) -> Option<super::inbox_idle::Offered> {
        let now = (self.clock)();
        let (home, principal) = self.account(now)?;
        let routines = match self.discovered(&home, &principal, now) {
            Ok(r) => r,
            Err(e) => {
                log::warn!(target: "routines", "routine discovery failed: {e}");
                return None;
            }
        };
        let store = match self.store(&home) {
            Ok(s) => s,
            Err(e) => {
                log::warn!(target: "routines", "routine store unavailable: {e}");
                return None;
            }
        };
        let mut state = self.inbox_idle.lock().unwrap_or_else(|e| e.into_inner());
        match super::inbox_idle::offer(
            &(home, principal),
            &routines,
            &store,
            &mut state,
            sessions,
            now,
        ) {
            Ok(offered) => {
                announce_admissions(&offered.admissions);
                Some(offered)
            }
            Err(e) => {
                log::warn!(target: "routines", "routine inbox_idle offer failed: {e}");
                None
            }
        }
    }

    /// The signed-in account, re-resolved when the last answer (a sign-in or
    /// nobody) is older than [`DISCOVERY_TTL_MS`].
    fn account(&self, now: i64) -> Option<Account> {
        let mut slot = self.account.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, account)) = slot.as_ref() {
            if (0..DISCOVERY_TTL_MS).contains(&(now - at)) {
                return account.clone();
            }
        }
        let account = (self.ambient)();
        *slot = Some((now, account.clone()));
        account
    }

    /// Every visible routine, rescanned when the cache is older than
    /// [`DISCOVERY_TTL_MS`], belongs to another account, or a local write
    /// invalidated it.
    fn discovered(
        &self,
        home: &Path,
        principal: &Principal,
        now: i64,
    ) -> Result<Arc<Vec<DiscoveredRoutine>>, RoutineError> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        // Read before scanning: a write that lands mid-scan leaves the cache
        // marked with the older generation, so the next wake rescans.
        let generation = discovery_generation(home);
        if let Some(c) = cache.as_ref() {
            let age = now - c.fetched_at;
            if c.home == *home
                && c.principal == *principal
                && c.generation == generation
                && (0..DISCOVERY_TTL_MS).contains(&age)
            {
                return Ok(Arc::clone(&c.routines));
            }
        }
        // Legacy routines an older client wrote since the last pass move under
        // their agents first (a no-op once done).
        super::discover::migrate_all(home, principal)?;
        let routines = Arc::new(discover_routines(home, principal)?);
        *cache = Some(CachedDiscovery {
            home: home.to_path_buf(),
            principal: principal.clone(),
            generation,
            fetched_at: now,
            routines: Arc::clone(&routines),
        });
        Ok(routines)
    }

    /// The run-lifecycle half of a wake: time out runs past `max_runtime`,
    /// release held runs a crash left waiting, and settle runs whose session
    /// liveness was unknown. Each part logs its own failure and never blocks
    /// the scheduling half.
    fn maintain(&self, now: i64, store: &RoutineStateStore) {
        if let Err(e) = finish::watchdog(store, now, self.run_sessions.as_ref()) {
            log::warn!(target: "routines", "routine watchdog failed: {e}");
        }
        let survivors = self
            .survivors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Err(e) = finish::sweep_exited(
            store,
            now,
            self.started_at,
            &survivors,
            self.run_sessions.as_ref(),
        ) {
            log::warn!(target: "routines", "routine exit sweep failed: {e}");
        }
        if let Err(e) = finish::release_stalled_queues(store) {
            log::warn!(target: "routines", "routine queue release failed: {e}");
        }
        let pending: Vec<String> = self
            .unknown_liveness
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .collect();
        if pending.is_empty() {
            return;
        }
        match finish::reconcile_runs(store, now, self.run_sessions.as_ref(), Some(&pending)) {
            Ok(report) => {
                self.recheck_liveness(report.unknown);
                self.watch_survivors(report.alive);
            }
            Err(e) => {
                log::warn!(target: "routines", "routine liveness re-check failed: {e}");
                self.recheck_liveness(pending);
            }
        }
    }

    fn store(&self, home: &Path) -> StoreResult<RoutineStateStore> {
        let mut slot = self.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((h, store)) = slot.as_ref() {
            if h == home {
                return Ok(store.clone());
            }
        }
        let store = RoutineStateStore::open(home)?;
        *slot = Some((home.to_path_buf(), store.clone()));
        Ok(store)
    }
}

fn announce_admissions(admissions: &[(String, Admission)]) {
    let changed = admissions
        .iter()
        .filter(|(_, a)| a.outcome != Outcome::Duplicate)
        .map(|(key, a)| (key.clone(), a.run_id.clone()))
        .collect::<Vec<_>>();
    announce(&changed);
}

/// Emit `routines:changed` for each committed change, and wake the claim loop
/// when any of them may be new work.
fn announce(changed: &[(String, String)]) {
    for (key, run_id) in changed {
        super::events::changed(key, run_id);
    }
    if !changed.is_empty() {
        crate::commands::sprint_run::claim_loop::wake();
    }
}

/// The scheduler wake: runs the app's [`RoutineTicker`], if one is managed.
pub fn run_routine_tick<R: Runtime>(app: &AppHandle<R>) {
    if let Some(ticker) = app.try_state::<RoutineTicker>() {
        ticker.tick();
    }
}

/// Resolve the signed-in account from the scheduler thread by bridging the
/// async resolver onto the runtime, bounded so a wedged auth read cannot park
/// the thread.
fn ambient_blocking(app: &AppHandle) -> Option<Account> {
    let app = app.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    tauri::async_runtime::spawn(async move {
        let _ = tx.send(
            crate::commands::route_to_context::resolve_ambient(&app)
                .await
                .ok(),
        );
    });
    let (home, user_id) = rx.recv_timeout(AMBIENT_WAIT).ok().flatten()?;
    Some((home, Principal::from(user_id)))
}

/// One pass over `routines` at `now`. Only routines that run here and are
/// switched on here are evaluated: a shared job armed on this Mac and not
/// paused, or a recipe each Mac runs that this Mac turned on. The cursors of
/// discovered routines that are not (paused or off here, armed on another
/// Mac) or of triggers that were edited away are
/// dropped, so one that comes back is seeded from that moment instead of
/// replaying the gap. A routine missing from discovery keeps its cursor: a
/// definition that is briefly unreadable mid-sync must not lose a due
/// occurrence. Another account's cursors are never touched.
pub fn run_routine_tick_with(
    now: i64,
    started_at: i64,
    account: &Account,
    routines: &[DiscoveredRoutine],
    store: &RoutineStateStore,
    admitter: &dyn Admit,
) -> StoreResult<TickReport> {
    let mut report = TickReport::default();
    // A schedule's cursor belongs to this Mac, not to whoever seeded it: the
    // next wake picks up where the last one left off, whichever personal space
    // it runs from, so no occurrence is lost or fired twice in between.
    let cursors: HashMap<String, (Option<i64>, String)> = store
        .triggers()?
        .into_iter()
        .map(|t| (t.trigger_id, (t.next_run_at, t.spec_hash)))
        .collect();
    let host =
        super::host::local_host_id(&account.0).map_err(crate::coordinator::StoreError::Io)?;
    // Discovery may predate a mode edit and a local pause. Settle only from
    // the definition reread under the same routine lock as edits and switches,
    // before taking any SQLite transaction (the service's lock order). The
    // store's CAS still protects against concurrent switch-row updates.
    for routine in routines {
        let settled =
            super::defs::with_routine_lock(&routine.dir, || -> Result<(), RoutineError> {
                if let Some(def) = super::defs::read_def(&routine.dir)? {
                    store.settle_switches(&[(routine.key.to_string(), &def)], &host)?;
                }
                Ok(())
            });
        if let Err(e) = settled {
            log::warn!(target: "routines", "routine {}: switch not settled: {e}", routine.key);
        }
    }
    let mut in_scope = HashSet::new();
    let mut live = HashSet::new();
    // SW-AGR-8: a routine moved off a machine fires nowhere else until that
    // machine acknowledges the disarm (it may still be firing it there): its
    // handover here, or as its file says to any Mac. Unreadable handovers
    // hold every routine that was ever on a machine (fails closed).
    let handing_over = super::machine_arm::pending_handovers(store);
    if let Err(error) = &handing_over {
        log::warn!(target: "routines", "routine handovers unreadable; holding routines that were on a machine: {error}");
    }
    let held_by_handover = |routine: &DiscoveredRoutine| {
        routine.def.handover_from.is_some()
            || match &handing_over {
                Ok(pending) => pending.contains(&routine.key.to_string()),
                Err(_) => routine.def.arm_epoch > 0,
            }
    };
    for routine in routines {
        if !routine.armed_here {
            in_scope.insert(routine.key.to_string());
            continue;
        }
        let enabled = match store.is_enabled(&routine.key, &routine.def, &host) {
            Ok(enabled) => enabled,
            Err(e) => {
                log::warn!(target: "routines", "routine {}: state unreadable: {e}", routine.key);
                continue;
            }
        };
        in_scope.insert(routine.key.to_string());
        // An Unassigned routine (its agent is nowhere here) is held like a
        // paused one: nothing fires it, and an agent picked later starts it
        // from that moment.
        if !enabled || routine.held() || held_by_handover(routine) {
            continue;
        }
        for (index, trigger) in routine.def.triggers.iter().enumerate() {
            let RoutineTrigger::Schedule(spec) = trigger else {
                continue;
            };
            let trigger_id = format!("{}#{index}", routine.key);
            live.insert(trigger_id.clone());
            let cursor = cursors.get(&trigger_id);
            let step = TriggerStep {
                trigger_id: &trigger_id,
                routine,
                cron: &spec.cron,
                tz: &spec.tz,
                cursor,
            };
            // One trigger's failure must not stall every routine after it.
            if let Err(e) = step.run(now, started_at, account, store, admitter, &mut report) {
                log::warn!(target: "routines", "routine {}: tick failed: {e}", routine.key);
            }
        }
    }
    store.prune_triggers(&in_scope, &live)?;
    prune_gone_routines(account, store, &in_scope)?;
    Ok(report)
}

/// Drop the cursors of routines that are no longer there to fire: archived,
/// or whose definition was deleted. A routine merely missing from this wake's
/// discovery (a file unreadable mid-sync, a team not scanned) keeps its cursor,
/// and so does every cursor another personal space on this Mac seeded: this
/// space's clone of that routine may be behind, and that space's own wake
/// prunes it.
fn prune_gone_routines(
    account: &Account,
    store: &RoutineStateStore,
    in_scope: &HashSet<String>,
) -> StoreResult<()> {
    let (home, principal) = account;
    let absent: HashSet<String> = store
        .triggers()?
        .into_iter()
        .filter(|t| t.owner == principal.as_str())
        .map(|t| t.routine_key)
        .filter(|key| !in_scope.contains(key))
        .collect();
    let gone: HashSet<String> = absent
        .into_iter()
        .filter(|key| {
            let Some(parsed) = RoutineKey::parse(key) else {
                return true;
            };
            let org_root = match crate::context_repo::path_to_org_root(home, principal, &parsed.org)
            {
                Ok(root) => root,
                Err(error) => {
                    log::warn!("{error}");
                    return false;
                }
            };
            // An org not cloned here is not scanned, so its routine is merely
            // missing.
            if !org_root.is_dir() {
                return false;
            }
            let Ok(config_dir) = super::service::key_config_dir(home, principal, &parsed) else {
                return false;
            };
            let Some(dir) = super::service::find_dir(&config_dir, &parsed.id) else {
                return true;
            };
            match super::defs::read_def(&dir) {
                Ok(None) => true,
                Ok(Some(def)) => def.is_archived(),
                Err(_) => false,
            }
        })
        .collect();
    if !gone.is_empty() {
        store.prune_triggers(&gone, &HashSet::new())?;
    }
    Ok(())
}

/// One schedule trigger of one live routine in a pass.
struct TriggerStep<'a> {
    trigger_id: &'a str,
    routine: &'a DiscoveredRoutine,
    cron: &'a str,
    tz: &'a str,
    cursor: Option<&'a (Option<i64>, String)>,
}

impl TriggerStep<'_> {
    fn run(
        &self,
        now: i64,
        started_at: i64,
        account: &Account,
        store: &RoutineStateStore,
        admitter: &dyn Admit,
        report: &mut TickReport,
    ) -> StoreResult<()> {
        let principal = &account.1;
        let hash = spec_hash(self.cron, self.tz);
        let due = match self.cursor {
            Some((cursor, h)) if *h == hash => *cursor,
            // New, or the spec changed: start from now so an edit never fires
            // an occurrence that predates it. A spec that would be refused at
            // save (a hand edit, a newer build's syntax) is parked with no
            // cursor, so it neither fires nor warns again until it changes.
            _ => {
                // SW-AGR-8: back from a machine, it starts after the last
                // occurrence the machine fired (its clock may run ahead).
                let from = super::machine_arm::fired_floor(store, &self.routine.key.to_string())
                    .ok()
                    .flatten()
                    .map_or(now, |floor| floor.max(now));
                let next = match schedule::validate_at(self.cron, self.tz, at(now)) {
                    Ok(()) => CompiledSchedule::parse(self.cron, self.tz)
                        .ok()
                        .and_then(|s| s.next_after(at(from)))
                        .map(|t| t.timestamp_millis()),
                    Err(e) => {
                        log::warn!(
                            target: "routines",
                            "routine {} has an unusable schedule and will not fire: {e}",
                            self.routine.key
                        );
                        None
                    }
                };
                store.seed_trigger(self.trigger_id, &self.routine.key, next, &hash, principal)?;
                report.seeded += 1;
                return Ok(());
            }
        };
        let Some(due) = due.filter(|due| *due <= now) else {
            return Ok(());
        };
        let schedule = match CompiledSchedule::parse(self.cron, self.tz) {
            Ok(schedule) => schedule,
            Err(e) => {
                // A spec this build can no longer read (a zone dropped from the
                // tz database): park it like a refused seed, warning once.
                log::warn!(
                    target: "routines",
                    "routine {} has an unusable schedule and will not fire: {e}",
                    self.routine.key
                );
                store.claim(self.trigger_id, due, None, |_| Ok(()))?;
                return Ok(());
            }
        };
        claim_due(
            &DueTrigger {
                trigger_id: self.trigger_id,
                routine: self.routine,
                schedule: &schedule,
                due,
            },
            now,
            started_at,
            account,
            store,
            admitter,
            report,
        )
    }
}

struct DueTrigger<'a> {
    trigger_id: &'a str,
    routine: &'a DiscoveredRoutine,
    schedule: &'a CompiledSchedule,
    /// The cursor value, at or before now.
    due: i64,
}

/// Claim one due cursor and apply the missed policy.
///
/// The occurrences are the cursor itself plus every fire up to `now`. When the
/// latest of them fell while this process was running, the app was only late
/// (App Nap, a sleeping laptop): that latest occurrence runs, whatever the
/// missed policy, and any earlier ones are recorded as one skipped row. The
/// missed policy decides only when every occurrence fell before this process
/// started, i.e. while the app was closed. A pile is never replayed.
fn claim_due(
    t: &DueTrigger<'_>,
    now: i64,
    started_at: i64,
    account: &Account,
    store: &RoutineStateStore,
    admitter: &dyn Admit,
    report: &mut TickReport,
) -> StoreResult<()> {
    // Only the count and the last two occurrences matter, so a long downtime
    // is walked without being collected.
    let mut n = 1usize;
    let mut latest = at(t.due);
    let mut previous = None;
    let mut next = t.schedule.next_after(latest);
    while let Some(fire) = next.filter(|f| f.timestamp_millis() <= now) {
        n += 1;
        previous = Some(latest);
        latest = fire;
        next = t.schedule.next_after(fire);
    }
    let next = next.map(|f| f.timestamp_millis());
    let key = &t.routine.key;
    let occurrence = |source: RunSource, when: DateTime<Utc>| Occurrence {
        routine_key: key.clone(),
        revision: t.routine.def.revision,
        source,
        occurrence_key: occurrence_key(key, when),
        scheduled_at: when.timestamp_millis(),
        claimed_at: now,
    };
    let late_in_session = latest.timestamp_millis() >= started_at;
    let mut admitted = 0;
    let mut skipped = 0;
    let mut changed = Vec::new();
    let claimed = store.claim(t.trigger_id, t.due, next, |tx| {
        // A second trigger of the same routine landing on the same instant
        // finds the first one's row and admits nothing.
        let mut admit_once = |occurrence: &Occurrence| -> StoreResult<()> {
            let admission = admitter.admit(tx, account, t.routine, occurrence)?;
            if admission.is_queued() {
                admitted += 1;
            }
            if admission.outcome != Outcome::Duplicate {
                changed.push((key.to_string(), admission.run_id));
            }
            Ok(())
        };
        let missed = if late_in_session {
            admit_once(&occurrence(RunSource::Schedule, latest))?;
            previous.map(|p| (p, n - 1))
        } else {
            match t.routine.def.policy.missed {
                MissedPolicy::Skip => Some((latest, n)),
                MissedPolicy::RunOnce => {
                    admit_once(&occurrence(RunSource::CatchUp, latest))?;
                    previous.map(|p| (p, n - 1))
                }
            }
        };
        if let Some((when, count)) = missed {
            // A second trigger landing on the same instant already recorded
            // it; count each missed occurrence once.
            if let Some(run_id) = record_skipped(tx, &occurrence(RunSource::Schedule, when), count)?
            {
                add_missed(tx, key, count)?;
                skipped = count;
                changed.push((key.to_string(), run_id));
            }
        }
        Ok(())
    })?;
    if claimed {
        report.admitted += admitted;
        report.skipped += skipped;
        report.changed.extend(changed);
    }
    Ok(())
}

/// One `skipped` row standing for `count` missed occurrences, keyed apart from
/// the occurrence itself (see [`missed_key`]) so it never stands in the way of
/// another trigger's run at that instant. `None` when the row already exists.
fn record_skipped(
    tx: &Transaction<'_>,
    occurrence: &Occurrence,
    count: usize,
) -> rusqlite::Result<Option<String>> {
    let run_id = uuid::Uuid::new_v4().to_string();
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO routine_runs
             (run_id, routine_key, revision, source, occurrence_key, phase, reason, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'skipped', ?6, ?7)",
        params![
            run_id,
            occurrence.routine_key.to_string(),
            occurrence.revision,
            occurrence.source.as_str(),
            missed_key(&occurrence.occurrence_key),
            format!("missed:{count}"),
            occurrence.claimed_at,
        ],
    )?;
    Ok((inserted > 0).then_some(run_id))
}

/// The key of the skipped row recording missed occurrences up to
/// `occurrence_key`. Two triggers of one routine can share an instant, so a
/// missed row keyed like a run would suppress the other trigger's run there.
pub fn missed_key(occurrence_key: &str) -> String {
    format!("{occurrence_key}#missed")
}

fn add_missed(tx: &Transaction<'_>, key: &RoutineKey, count: usize) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO routine_state (routine_key, missed_count) VALUES (?1, ?2)
         ON CONFLICT(routine_key) DO UPDATE SET missed_count = missed_count + excluded.missed_count",
        params![key.to_string(), i64::try_from(count).unwrap_or(i64::MAX)],
    )?;
    Ok(())
}

/// `<routine_key>@<instant>`. Routine ids are only unique per team, so the key
/// carries the org and team.
pub fn occurrence_key(key: &RoutineKey, when: DateTime<Utc>) -> String {
    format!("{key}@{}", when.to_rfc3339_opts(SecondsFormat::Secs, true))
}

pub(super) fn spec_hash(cron: &str, tz: &str) -> String {
    let normalized = format!(
        "{}\n{}",
        cron.split_whitespace().collect::<Vec<_>>().join(" "),
        tz.trim()
    );
    let digest = Sha256::digest(normalized.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn at(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_default()
}
