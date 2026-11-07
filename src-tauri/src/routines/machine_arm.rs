// SPDX-License-Identifier: MIT
//! SW-AGR-8, the desktop half: routines armed on a machine.
//!
//! A routine whose definition names a machine (`armed_machine`) fires on that
//! machine's session server, on its own clock, whether or not any desktop is
//! connected (`RoutineArmV1`). No Mac fires it (`host::is_armed_here`). The
//! Mac its `armed_host` names *manages* the arm:
//!
//! - **Push.** Whenever it can reach the machine (on connect, and on every
//!   scheduler pass while connected), it pushes the routine's pinned revision
//!   under the routine's arm epoch. The machine keeps the schedule's cursor
//!   across an edit.
//! - **Prepare (SW-XS-25).** The agent's workspace on the machine is prepared
//!   (SW-XS-7) only by an explicit action: arming, editing, resuming or
//!   restoring the routine, releasing its handover, or "Refresh on machine"
//!   ([`prepare_refresh`]). The launch recipe it makes is kept here
//!   ([`prepared`]). The background sync never prepares (rule X1): it pushes
//!   the routine's definition as it is now on the kept recipe, so an edit
//!   reaches the machine, and when the agent's content drifted from what was
//!   prepared it marks the arm `content_stale`; the machine starts the agent
//!   from what was last prepared until a refresh.
//! - **Disarm.** A routine it manages that the machine still holds but the
//!   definition no longer arms there (moved back to a Mac or to another
//!   machine, archived, paused) is disarmed at the definition's epoch.
//! - **Handover.** Moving a routine off a machine records a pending handover,
//!   here and in the routine's file (`handover_from`), so every Mac holds it.
//!   Until that machine acknowledges the disarm, nothing else fires the
//!   routine: not a Mac, not another machine. A machine that is offline
//!   keeps firing it meanwhile (it is still armed there), so the move never
//!   doubles a fire and never loses the machine's runs. A machine missing
//!   from the roster may still be running: its handovers wait, shown as
//!   waiting, until it acknowledges or a person releases them because the
//!   machine is gone. The last occurrence the machine fired comes back with
//!   its acknowledgement: this Mac's schedule starts after it.
//! - **Runs.** It reads each machine's run log from a persisted cursor and
//!   records the current managing Mac's routines as `routine_runs` rows,
//!   including their history from earlier epochs. A takeover rewinds that
//!   Mac's cursor; the machine's stable run ids keep replay idempotent.
//!
//! The machine fences every arm and disarm by epoch, so a desktop holding a
//! stale definition can never re-arm a routine that moved.

use codesense_host_proto::agent_cli::NativeLaunch;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use codesense_pty_core::routine_arm::{
    ArmAnswer, ArmInfo, ArmLaunch, ArmList, ArmMissed, ArmMode, ArmOverlap, ArmRefusal, ArmRun,
    ArmRunLog, ArmRunStatus, ArmSchedule, ArmTarget, RoutineArm,
};
use codesense_pty_core::session_wake::Originator;
use rusqlite::{params, OptionalExtension};

use crate::coordinator::StoreResult;
use crate::domain::Principal;

use super::discover::DiscoveredRoutine;
use super::model::{MissedPolicy, OverlapPolicy, RoutineMode, RoutineTrigger, Variable};
use super::runs::phase;
use super::state::RoutineStateStore;

/// Where a machine-armed routine runs on its machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArmPlace {
    /// The machine workspace (`remoteWorkspaceId`).
    pub workspace_id: String,
    /// Its prepared workspace handle.
    pub handle: String,
    /// Its directory on the machine.
    pub cwd: String,
}

/// A machine's `RoutineArmV1` surface.
pub(crate) trait ArmHost: Send + Sync {
    fn arms(&self) -> Result<ArmList, String>;
    fn arm(&self, arm: RoutineArm) -> Result<ArmAnswer, String>;
    fn disarm(&self, key: &str, epoch: u64) -> Result<ArmAnswer, String>;
    fn runs(&self, since: u64) -> Result<ArmRunLog, String>;
    /// SW-AGR-13: its terminal server enforces a routine's wake limits
    /// (proto minor 13 and later). One that does not would fire a limited
    /// routine without them.
    fn enforces_wake_limits(&self) -> bool {
        true
    }
    /// SW-AGR-11: the identity this desktop bound its connection with, when
    /// the machine keeps it (proto minor 14 and later): an arm stamped
    /// otherwise is sent again, so its agents come back to this desktop.
    fn desktop_identity(&self) -> Option<String> {
        None
    }
}

/// Where a machine's terminal server stands right now.
pub(crate) enum ArmLookup {
    Ready(Arc<dyn ArmHost>),
    /// Configured but not reachable: nothing changes until it is.
    Offline,
    /// Reachable, but its terminal server predates `RoutineArmV1`.
    Unsupported(String),
    /// Not in the roster (removed, or never shared). It may still be
    /// running: its handovers keep waiting until a person releases them.
    Unknown,
}

/// What a routine's agent is, as the arm carries it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct AgentFacts {
    /// Its manifest (what a preparation writes).
    pub manifest_hash: String,
    /// It set `allowWake: true`.
    pub allow_wake: bool,
}

/// What the app gives the sync: machines, and where a routine's agent runs on
/// one. It cannot prepare: the background sync never writes a workspace.
pub(crate) trait ArmEnv: Send + Sync {
    fn host(&self, machine: &str) -> ArmLookup;
    /// The routine's workspace on `machine`.
    fn place(&self, routine: &DiscoveredRoutine, machine: &str) -> Result<ArmPlace, String>;
}

/// SW-XS-25: preparation, handed only to [`prepare_refresh`], which only
/// explicit actions call.
pub(crate) trait ArmPrepare: Send + Sync {
    /// Prepare the routine's agent into `place` on `machine` (SW-XS-7) and
    /// say how the machine starts it.
    fn prepare(
        &self,
        routine: &DiscoveredRoutine,
        machine: &str,
        place: &ArmPlace,
    ) -> Result<ArmLaunch, String>;
}

/// What one sync with a machine did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SyncReport {
    pub armed: Vec<String>,
    pub disarmed: Vec<String>,
    /// Arms the machine refused as older than what it holds.
    pub stale: Vec<String>,
    /// Handovers the machine acknowledged (or that no longer apply).
    pub handed_over: Vec<String>,
    /// Handovers waiting on a machine missing from the roster (it may still
    /// fire them): released only by a person.
    pub waiting: Vec<String>,
    /// Run log entries recorded as run rows.
    pub recorded: usize,
    /// Why a routine could not be prepared there (it is armed without a
    /// launch recipe: fresh fires are `needs_preparation` until it can be).
    pub unprepared: Vec<(String, String)>,
    /// SW-AGR-13: routines with wake limits its terminal server is too old to
    /// enforce: not armed there (disarmed if they were) until it is updated.
    pub outdated: Vec<String>,
    /// SW-XS-25: routines whose arm there is not prepared from what they are
    /// now (their agent or definition changed, or nothing was prepared yet).
    /// Marked `content_stale`; only an explicit refresh prepares them.
    pub content_stale: Vec<String>,
    pub errors: Vec<String>,
}

/// SW-XS-25: why a machine arm is `content_stale`.
pub(crate) const STALE_DRIFTED: &str = "changed since it was prepared on the machine";
pub(crate) const STALE_UNPREPARED: &str = "not prepared on the machine yet";
pub(crate) const STALE_OTHER_AGENT: &str =
    "its agent changed since it was prepared on the machine; it needs preparing there";
pub(crate) const STALE_AGENT_UNKNOWN: &str =
    "what was prepared on the machine does not say for which agent; it needs preparing there";

/// A machine arm whose prepared content is behind (SW-XS-25).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ContentStale {
    pub machine: String,
    pub since_ms: i64,
    pub why: String,
}

/// Mark `key`'s arm on `machine` stale. A first mark (or a new reason) tells
/// the views.
pub(crate) fn mark_stale(
    store: &RoutineStateStore,
    key: &str,
    machine: &str,
    why: &str,
    now: i64,
) -> StoreResult<()> {
    let changed = store.transaction(|tx| {
        Ok(tx.execute(
            "INSERT INTO routine_arm_stale (routine_key, machine, since_ms, why)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(routine_key, machine) DO UPDATE SET why = ?4, since_ms = ?3
             WHERE why != ?4",
            params![key, machine, now, why],
        )?)
    })?;
    if changed > 0 {
        super::events::routine_changed(key);
    }
    Ok(())
}

/// `key`'s arm on `machine` is prepared from what it is now.
pub(crate) fn clear_stale(store: &RoutineStateStore, key: &str, machine: &str) -> StoreResult<()> {
    let changed = store.transaction(|tx| {
        Ok(tx.execute(
            "DELETE FROM routine_arm_stale WHERE routine_key = ?1 AND machine = ?2",
            params![key, machine],
        )?)
    })?;
    if changed > 0 {
        super::events::routine_changed(key);
    }
    Ok(())
}

/// What this Mac last prepared for a routine on a machine (SW-XS-25): the
/// launch recipe a definition-only push carries, so an edit reaches the
/// machine without a preparation and the machine keeps starting the agent
/// from what was prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prepared {
    /// The machine workspace it was prepared in.
    pub workspace_id: String,
    /// The agent it was prepared for (its key). Empty for a recipe kept
    /// before this was recorded: for no agent this Mac can vouch for.
    pub agent_key: String,
    /// The agent's manifest it was prepared from ([`AgentFacts`]).
    pub manifest_hash: String,
    pub launch: ArmLaunch,
}

/// The recipe last prepared for `key` on `machine`, if any.
pub(crate) fn prepared(
    store: &RoutineStateStore,
    key: &str,
    machine: &str,
) -> StoreResult<Option<Prepared>> {
    let row: Option<(String, String, String, String)> = store.transaction(|tx| {
        Ok(tx
            .query_row(
                "SELECT workspace_id, agent_key, manifest_hash, launch_json
                   FROM routine_arm_prepared WHERE routine_key = ?1 AND machine = ?2",
                params![key, machine],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?)
    })?;
    // A recipe this build cannot read is no recipe: the next refresh
    // replaces it.
    Ok(
        row.and_then(|(workspace_id, agent_key, manifest_hash, json)| {
            serde_json::from_str(&json).ok().map(|launch| Prepared {
                workspace_id,
                agent_key,
                manifest_hash,
                launch,
            })
        }),
    )
}

fn save_prepared(
    store: &RoutineStateStore,
    key: &str,
    machine: &str,
    prepared: &Prepared,
    now: i64,
) -> StoreResult<()> {
    let json = serde_json::to_string(&prepared.launch).unwrap_or_default();
    store.transaction(|tx| {
        tx.execute(
            "INSERT INTO routine_arm_prepared
                 (routine_key, machine, workspace_id, manifest_hash, launch_json, prepared_ms,
                  agent_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(routine_key, machine) DO UPDATE SET workspace_id = ?3,
                 manifest_hash = ?4, launch_json = ?5, prepared_ms = ?6, agent_key = ?7",
            params![
                key,
                machine,
                prepared.workspace_id,
                prepared.manifest_hash,
                json,
                now,
                prepared.agent_key
            ],
        )?;
        Ok(())
    })
}

/// Drop the recipe prepared for `key` on `machine` (it is for another agent).
fn drop_prepared(store: &RoutineStateStore, key: &str, machine: &str) -> StoreResult<()> {
    store.transaction(|tx| {
        tx.execute(
            "DELETE FROM routine_arm_prepared WHERE routine_key = ?1 AND machine = ?2",
            params![key, machine],
        )?;
        Ok(())
    })
}

/// Clear `key`'s stale mark on `machine` only while it is still `why` since
/// `since_ms` (a later mark, for any reason, stays).
pub(crate) fn clear_stale_if(
    store: &RoutineStateStore,
    key: &str,
    machine: &str,
    why: &str,
    since_ms: i64,
) -> StoreResult<()> {
    let changed = store.transaction(|tx| {
        Ok(tx.execute(
            "DELETE FROM routine_arm_stale
              WHERE routine_key = ?1 AND machine = ?2 AND why = ?3 AND since_ms = ?4",
            params![key, machine, why, since_ms],
        )?)
    })?;
    if changed > 0 {
        super::events::routine_changed(key);
    }
    Ok(())
}

/// Forget `key`'s arm state on every machine but `keep` (deleted, archived,
/// or moved): its stale mark and its prepared recipe.
pub(crate) fn forget_machine_arms(
    store: &RoutineStateStore,
    key: &str,
    keep: Option<&str>,
) -> StoreResult<()> {
    let keep = keep.unwrap_or("");
    let changed = store.transaction(|tx| {
        let stale = tx.execute(
            "DELETE FROM routine_arm_stale WHERE routine_key = ?1 AND machine != ?2",
            params![key, keep],
        )?;
        tx.execute(
            "DELETE FROM routine_arm_prepared WHERE routine_key = ?1 AND machine != ?2",
            params![key, keep],
        )?;
        Ok(stale)
    })?;
    if changed > 0 {
        super::events::routine_changed(key);
    }
    Ok(())
}

/// `key`'s stale mark from `stale` (as [`stale_arms`] read it), only for the
/// machine `def` is armed on now.
pub(crate) fn stale_for(
    stale: &mut HashMap<String, ContentStale>,
    key: &str,
    def: &super::model::RoutineDef,
) -> Option<ContentStale> {
    stale
        .remove(key)
        .filter(|s| def.armed_machine() == Some(s.machine.as_str()))
}

/// Every stale machine arm, by routine key, or none when they cannot be read.
pub(crate) fn stale_arms_at(home: &std::path::Path) -> HashMap<String, ContentStale> {
    RoutineStateStore::open(home)
        .ok()
        .and_then(|store| stale_arms(&store).ok())
        .unwrap_or_default()
}

/// Every stale machine arm, by routine key.
pub(crate) fn stale_arms(store: &RoutineStateStore) -> StoreResult<HashMap<String, ContentStale>> {
    store.transaction(|tx| {
        let mut stmt =
            tx.prepare("SELECT routine_key, machine, since_ms, why FROM routine_arm_stale")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    ContentStale {
                        machine: r.get(1)?,
                        since_ms: r.get(2)?,
                        why: r.get(3)?,
                    },
                ))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(rows)
    })
}

/// A routine moved off `machine` whose disarm it has not acknowledged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Handover {
    pub routine_key: String,
    pub machine: String,
    pub epoch: u64,
    pub since_ms: i64,
}

/// Record that `key` moved off `machine` at `epoch`: nothing else fires it
/// until that machine acknowledges the disarm.
pub(crate) fn begin_handover(
    store: &RoutineStateStore,
    key: &str,
    machine: &str,
    epoch: u64,
    now: i64,
) -> StoreResult<()> {
    store.transaction(|tx| {
        tx.execute(
            "INSERT INTO routine_arm_handover (routine_key, machine, epoch, since_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(routine_key, machine) DO UPDATE SET epoch = MAX(epoch, ?3), since_ms = ?4",
            params![key, machine, epoch as i64, now],
        )?;
        Ok(())
    })
}

/// End `key`'s handover off `machine` without its acknowledgement (a person
/// said the machine is gone).
pub(crate) fn release_handover(
    store: &RoutineStateStore,
    key: &str,
    machine: &str,
) -> StoreResult<()> {
    end_handover(store, key, machine)
}

/// The occurrence a machine last fired a routine at, as its acknowledgement
/// reported it: a schedule here starts after it.
pub(crate) fn fired_floor(store: &RoutineStateStore, key: &str) -> StoreResult<Option<i64>> {
    store.transaction(|tx| {
        Ok(tx
            .query_row(
                "SELECT last_fired_ms FROM routine_arm_floor WHERE routine_key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    })
}

fn set_fired_floor(store: &RoutineStateStore, key: &str, at: u64) -> StoreResult<()> {
    store.transaction(|tx| {
        tx.execute(
            "INSERT INTO routine_arm_floor (routine_key, last_fired_ms) VALUES (?1, ?2)
             ON CONFLICT(routine_key) DO UPDATE SET last_fired_ms = MAX(last_fired_ms, ?2)",
            params![key, at as i64],
        )?;
        Ok(())
    })
}

/// When a machine's run log last overflowed past this Mac's cursor (runs it
/// fired were dropped before this Mac read them), if it ever did.
pub(crate) fn runs_lost_at(store: &RoutineStateStore, machine: &str) -> StoreResult<Option<i64>> {
    store.transaction(|tx| {
        Ok(tx
            .query_row(
                "SELECT lost_at_ms FROM routine_arm_cursors WHERE machine = ?1",
                params![machine],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    })
}

pub(crate) fn handovers(store: &RoutineStateStore) -> StoreResult<Vec<Handover>> {
    store.transaction(|tx| {
        let mut stmt = tx.prepare(
            "SELECT routine_key, machine, epoch, since_ms FROM routine_arm_handover ORDER BY routine_key",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Handover {
                    routine_key: r.get(0)?,
                    machine: r.get(1)?,
                    epoch: r.get::<_, i64>(2)? as u64,
                    since_ms: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
}

/// The keys of every routine waiting on a machine's disarm. An error is
/// returned as is: a caller that cannot read them holds every routine that
/// was ever on a machine (fails closed).
pub(crate) fn pending_handovers(store: &RoutineStateStore) -> StoreResult<HashSet<String>> {
    handovers(store).map(|rows| rows.into_iter().map(|h| h.routine_key).collect())
}

fn end_handover(store: &RoutineStateStore, key: &str, machine: &str) -> StoreResult<()> {
    store.transaction(|tx| {
        tx.execute(
            "DELETE FROM routine_arm_handover WHERE routine_key = ?1 AND machine = ?2",
            params![key, machine],
        )?;
        Ok(())
    })
}

fn cursor(store: &RoutineStateStore, machine: &str) -> StoreResult<u64> {
    store.transaction(|tx| {
        Ok(tx
            .query_row(
                "SELECT last_seq FROM routine_arm_cursors WHERE machine = ?1",
                params![machine],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0) as u64)
    })
}

/// A new managing Mac may already have read and skipped the routine's
/// history. Replay it under the new owner, keeping each machine run's id.
pub(crate) fn rewind_run_cursors(
    store: &RoutineStateStore,
    from: Option<&str>,
    to: Option<&str>,
) -> StoreResult<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE routine_arm_cursors SET last_seq = 0 WHERE machine = ?1 OR machine = ?2",
            params![from, to],
        )?;
        Ok(())
    })
}

fn save_cursor(
    store: &RoutineStateStore,
    machine: &str,
    seq: u64,
    boot: &str,
    lost_at: Option<i64>,
) -> StoreResult<()> {
    store.transaction(|tx| {
        tx.execute(
            "INSERT INTO routine_arm_cursors (machine, last_seq, boot_id, lost_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(machine) DO UPDATE SET last_seq = ?2, boot_id = ?3,
                 lost_at_ms = COALESCE(?4, lost_at_ms)",
            params![machine, seq as i64, boot, lost_at],
        )?;
        Ok(())
    })
}

/// What the arm carries from this Mac's own record of the routine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Carried {
    /// The last occurrence this Mac fired it at: the machine fires nothing
    /// at or before it.
    pub last_fired_ms: Option<u64>,
    pub last_run_summary: Option<String>,
    pub last_run_at_ms: Option<u64>,
    /// This Mac's wake budget generation for it (SW-AGR-13): the arm's limits
    /// count under it, and a resume starts a new one.
    pub wake_budget: u64,
}

/// The arm a routine this Mac manages carries to `machine`: its pinned
/// revision, under its epoch, with `launch` when its agent is prepared there.
/// Its task has its declared variables rendered from their defaults (the
/// machine renders the built-ins); its `source_digest` names everything it
/// was built from, so a change to any of it is pushed again.
#[allow(clippy::too_many_arguments)]
pub(crate) fn desired_arm(
    routine: &DiscoveredRoutine,
    mode: RoutineMode,
    place: &ArmPlace,
    owner: &str,
    host_id: &str,
    author: &str,
    agent: &AgentFacts,
    carried: &Carried,
    launch: Option<ArmLaunch>,
    limits_enforced: bool,
) -> Result<RoutineArm, String> {
    let def = &routine.def;
    let agent_key = routine
        .agent_key
        .as_ref()
        .ok_or("the routine has no agent this Mac can see")?;
    let schedules: Vec<ArmSchedule> = def
        .triggers
        .iter()
        .filter_map(|trigger| match trigger {
            RoutineTrigger::Schedule(spec) => Some(ArmSchedule {
                cron: spec.cron.clone(),
                tz: spec.tz.clone(),
            }),
            _ => None,
        })
        .collect();
    let mut arm = RoutineArm {
        key: routine.key.to_string(),
        epoch: def.arm_epoch.max(1),
        revision: def.revision,
        name: def.name.clone(),
        mode: match mode {
            RoutineMode::Wake => ArmMode::Wake,
            RoutineMode::Fresh => ArmMode::Fresh,
        },
        schedules,
        task: render_declared(&def.task, &def.variables, &Default::default()),
        target: ArmTarget {
            agent: agent_key.to_string(),
            workspace_id: place.workspace_id.clone(),
            cwd: place.cwd.clone(),
            owner: Some(owner.to_string()).filter(|o| !o.is_empty()),
            allow_wake: agent.allow_wake,
        },
        max_wait_ms: super::wake::max_wait_ms(def),
        overlap: match def.policy.overlap {
            OverlapPolicy::Skip => ArmOverlap::Skip,
            OverlapPolicy::Coalesce | OverlapPolicy::Queue => ArmOverlap::Coalesce,
        },
        missed: match def.policy.missed {
            MissedPolicy::Skip => ArmMissed::Skip,
            MissedPolicy::RunOnce => ArmMissed::RunOnce,
        },
        launch: None,
        originator: Originator {
            desktop: String::new(),
            routine_host: Some(host_id.to_string()),
        },
        author: author.to_string(),
        max_runtime_ms: def
            .policy
            .max_runtime_duration()
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        last_fired_ms: carried.last_fired_ms,
        last_run_summary: carried.last_run_summary.clone(),
        last_run_at_ms: carried.last_run_at_ms,
        source_digest: String::new(),
        // Only a wake delivers into a session: a fresh run counts no wakes.
        limits: (mode == RoutineMode::Wake)
            .then(|| {
                super::wake::wake_limits_of(
                    def,
                    &routine.key.to_string(),
                    host_id,
                    carried.wake_budget,
                )
            })
            .flatten(),
    };
    arm.source_digest = source_digest(&arm, &agent.manifest_hash, limits_enforced);
    arm.launch = launch;
    Ok(arm)
}

/// A digest of what an arm was built from: everything it carries but its
/// launch recipe, the seeds that only matter once, and the stamp the
/// machine fills in, plus its agent's manifest. For an arm with wake limits it
/// also names whether the machine enforces them, so a terminal server that
/// was updated since gets the arm pushed again (SW-AGR-13).
fn source_digest(arm: &RoutineArm, manifest_hash: &str, limits_enforced: bool) -> String {
    let mut basis = arm.clone();
    basis.launch = None;
    basis.originator.desktop = String::new();
    basis.last_fired_ms = None;
    basis.last_run_summary = None;
    basis.last_run_at_ms = None;
    basis.source_digest = String::new();
    let json = serde_json::to_string(&basis).unwrap_or_default();
    let enforced = if arm.limits.is_some() && limits_enforced {
        "\0limits-enforced"
    } else {
        ""
    };
    blake3::hash(format!("{json}\0{manifest_hash}{enforced}").as_bytes())
        .to_hex()
        .to_string()
}

/// `task` with the declared `variables` rendered (defaults under `fired`'s
/// values); every other token (the built-ins) stays for the machine.
pub(crate) fn render_declared(
    task: &str,
    variables: &std::collections::BTreeMap<String, Variable>,
    fired: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut values: std::collections::BTreeMap<&str, String> = variables
        .iter()
        .filter_map(|(name, v)| Some((name.as_str(), v.default.clone()?)))
        .collect();
    for (name, value) in fired {
        if variables.contains_key(name) {
            values.insert(name.as_str(), value.clone());
        }
    }
    codesense_pty_core::routine_arm::render_static(task, &values)
}

/// Everything one sync needs besides the machine.
pub(crate) struct SyncInput<'a> {
    pub store: &'a RoutineStateStore,
    pub principal: &'a Principal,
    pub host_id: &'a str,
    pub routines: &'a [DiscoveredRoutine],
    /// What a fire of each routine does (its effective mode).
    pub mode_of: &'a dyn Fn(&DiscoveredRoutine) -> RoutineMode,
    /// Who last edited each routine, for the delivery marker.
    pub author_of: &'a dyn Fn(&DiscoveredRoutine) -> String,
    /// Read a routine this sync did not discover (archived or deleted): its
    /// definition, including owner and epoch, or `None` when it is gone.
    pub undiscovered: &'a dyn Fn(&str) -> Option<super::model::RoutineDef>,
    /// Each routine's agent as the arm carries it (`None`: unreadable, so
    /// the arm is left as it is).
    pub agent_of: &'a dyn Fn(&DiscoveredRoutine) -> Option<AgentFacts>,
    /// A handover off a machine was acknowledged: clear the routine's file.
    pub settled: &'a dyn Fn(&str, &str, u64),
    /// The human inbox, for a run that ended on a wake limit (SW-AGR-13).
    pub notify: &'a dyn super::wake::WakeNotify,
    pub now: i64,
}

/// Bring `machine`'s arms in line with the definitions this Mac manages,
/// settle its handovers, and record its new runs.
pub(crate) fn sync_machine(env: &dyn ArmEnv, input: &SyncInput<'_>, machine: &str) -> SyncReport {
    let mut report = SyncReport::default();
    let host = match env.host(machine) {
        ArmLookup::Ready(host) => host,
        ArmLookup::Unknown => {
            // Missing from the roster is not stopped: it may still be firing
            // them. They wait, shown as waiting, until it acknowledges or a
            // person releases them (`release_handover`).
            report.waiting = handovers(input.store)
                .unwrap_or_default()
                .into_iter()
                .filter(|h| h.machine == machine)
                .map(|h| h.routine_key)
                .collect();
            return report;
        }
        ArmLookup::Offline => return report,
        ArmLookup::Unsupported(why) => {
            report.errors.push(why);
            return report;
        }
    };
    let list = match host.arms() {
        Ok(list) => list,
        Err(error) => {
            report.errors.push(error);
            return report;
        }
    };
    // SW-AGR-13: a wake routine with limits is never armed where its machine
    // would fire it without them.
    let enforced = host.enforces_wake_limits();
    let limits_unenforced = |routine: &DiscoveredRoutine| {
        !enforced
            && routine.def.policy.has_wake_limits()
            && (input.mode_of)(routine) == RoutineMode::Wake
    };
    let by_key: HashMap<String, &DiscoveredRoutine> = input
        .routines
        .iter()
        .map(|routine| (routine.key.to_string(), routine))
        .collect();
    // Handovers that cannot be read hold everything (fails closed): nothing
    // is armed or settled until they can be.
    let pending = match handovers(input.store) {
        Ok(pending) => pending,
        Err(error) => {
            report
                .errors
                .push(format!("routine handovers unreadable: {error}"));
            return report;
        }
    };
    // 1. Disarm what this Mac manages that is no longer armed here.
    for held in &list.armed {
        let disarm_at = match by_key.get(&held.key) {
            Some(routine) => {
                let def = &routine.def;
                let manages = super::host::names_mac(def, input.host_id);
                if !manages && held.originator.routine_host.as_deref() != Some(input.host_id) {
                    continue; // Another Mac manages it.
                }
                // Still armed there: kept, unless this Mac manages it and
                // paused it (or its agent is gone). A Mac that armed it but no
                // longer manages it leaves it to the one that does.
                let still = def.armed_machine() == Some(machine)
                    && (!manages
                        || (enabled(input, routine)
                            && !routine.held()
                            && !limits_unenforced(routine)));
                if still {
                    continue;
                }
                // Its own epoch: a move took a new one (the machine accepts
                // it); paused or Unassigned in place, it is the held one.
                (def.arm_epoch.max(1), def.armed_machine() != Some(machine))
            }
            None => {
                if held.originator.routine_host.as_deref() != Some(input.host_id) {
                    continue;
                }
                match (input.undiscovered)(&held.key) {
                    // Archived: disarmed at its own epoch.
                    Some(def) if def.is_archived() => (def.arm_epoch.max(1), true),
                    // Deleted outright, from this Mac's arm: nothing can
                    // re-arm it, so its held epoch closes it.
                    None => (held.epoch, true),
                    // Present but not discovered (a team not scanned): leave it.
                    Some(_) => continue,
                }
            }
        };
        let (disarm_at, gone) = disarm_at;
        match host.disarm(&held.key, disarm_at) {
            Ok(ArmAnswer::Disarmed { .. }) => {
                // Nothing fires there now: nothing there is stale. A routine
                // that left this machine also drops what was prepared there.
                let forgotten = if gone {
                    forget_machine_arms(input.store, &held.key, None)
                } else {
                    clear_stale(input.store, &held.key, machine)
                };
                if let Err(error) = forgotten {
                    report.errors.push(format!("{}: {error}", held.key));
                }
                report.disarmed.push(held.key.clone());
            }
            Ok(ArmAnswer::Refused {
                refusal: ArmRefusal::Stale { .. },
            }) => report.stale.push(held.key.clone()),
            Ok(other) => report.errors.push(format!("{}: {other:?}", held.key)),
            Err(error) => report.errors.push(format!("{}: {error}", held.key)),
        }
    }
    // A current manager acknowledges retired placements at its own epoch
    // too, so the same worker fence protects their late outcomes.
    for tomb in &list.disarmed {
        let def = by_key
            .get(&tomb.key)
            .map(|r| r.def.clone())
            .or_else(|| (input.undiscovered)(&tomb.key));
        let Some(def) = def.filter(|def| {
            super::host::names_mac(def, input.host_id)
                && def.armed_machine() != Some(machine)
                && tomb.epoch < def.arm_epoch.max(1)
        }) else {
            continue;
        };
        match host.disarm(&tomb.key, def.arm_epoch.max(1)) {
            Ok(ArmAnswer::Disarmed { .. }) => report.disarmed.push(tomb.key.clone()),
            Ok(ArmAnswer::Refused {
                refusal: ArmRefusal::Stale { .. },
            }) => report.stale.push(tomb.key.clone()),
            Ok(other) => report.errors.push(format!("{}: {other:?}", tomb.key)),
            Err(error) => report.errors.push(format!("{}: {error}", tomb.key)),
        }
    }
    // 2. Settle the handovers off this machine.
    let armed_now = match host.arms() {
        Ok(list) => list,
        Err(error) => {
            report.errors.push(error);
            return report;
        }
    };
    for handover in pending.iter().filter(|h| h.machine == machine) {
        let held = armed_now
            .armed
            .iter()
            .find(|info| info.key == handover.routine_key);
        let tombstone = armed_now
            .disarmed
            .iter()
            .find(|tomb| tomb.key == handover.routine_key)
            .and_then(|tomb| tomb.last_fired_ms);
        let (done, fired) = match held {
            None => (true, tombstone),
            // Armed there again at a newer epoch: moved back meanwhile.
            Some(info) if info.epoch > handover.epoch => (true, None),
            Some(info) => match host.disarm(&handover.routine_key, handover.epoch) {
                Ok(ArmAnswer::Disarmed { last_fired_ms, .. }) => {
                    (true, last_fired_ms.or(info.last_fired_ms))
                }
                Ok(ArmAnswer::Refused {
                    refusal: ArmRefusal::Stale { held_epoch, .. },
                }) => (held_epoch > handover.epoch, None),
                Ok(_) | Err(_) => (false, None),
            },
        };
        if !done {
            continue;
        }
        // The last occurrence it fired there: a schedule here starts after.
        if let Some(fired) = fired {
            if let Err(error) = set_fired_floor(input.store, &handover.routine_key, fired) {
                report
                    .errors
                    .push(format!("{}: {error}", handover.routine_key));
                continue;
            }
        }
        if end_handover(input.store, &handover.routine_key, machine).is_ok() {
            (input.settled)(&handover.routine_key, machine, handover.epoch);
            report.handed_over.push(handover.routine_key.clone());
        }
    }
    let still_pending: HashSet<String> = match handovers(input.store) {
        Ok(rows) => rows.into_iter().map(|h| h.routine_key).collect(),
        Err(error) => {
            report
                .errors
                .push(format!("routine handovers unreadable: {error}"));
            return report;
        }
    };
    // 3. Push what this Mac manages that should be armed here: its
    // definition, with the launch recipe last prepared there. Never prepared
    // here (SW-XS-25): content drift is marked `content_stale`.
    for routine in input.routines {
        if !armable_here(input, routine, machine, &still_pending) {
            continue;
        }
        let key = routine.key.to_string();
        if limits_unenforced(routine) {
            // It fires nowhere meanwhile (no Mac fires a machine-armed
            // routine): said once a day, in the inbox and the log.
            report.outdated.push(key.clone());
            let about = format!(
                "outdated-arm\0{}\0{machine}\0{key}",
                input.store.path().display()
            );
            if super::wake::first_note_within(&about, 24 * 60 * 60 * 1000) {
                log::warn!(target: "routines", "{key}: not armed on {machine}: its terminal server is too old to enforce the routine's wake limits");
                input.notify.post(&super::wake::WakeNotice {
                    kind: super::wake::NoticeKind::ServerOutdated,
                    run_id: String::new(),
                    routine_key: key.clone(),
                    machine: machine.to_string(),
                    session: String::new(),
                    detail: String::new(),
                });
            }
            continue;
        }
        let Some(agent) = (input.agent_of)(routine) else {
            report
                .errors
                .push(format!("{key}: its agent cannot be read on this Mac"));
            continue;
        };
        let held = armed_now.armed.iter().find(|info| info.key == key);
        push_definition(
            host.as_ref(),
            env,
            input,
            routine,
            machine,
            &agent,
            enforced,
            held,
            &mut report,
        );
    }
    // 4. Record the runs it fired for this Mac.
    match record_runs(host.as_ref(), input, machine) {
        Ok((recorded, lost)) => {
            report.recorded = recorded;
            if lost {
                report.errors.push(format!(
                    "some routine runs {machine} fired were dropped from its log before this Mac read them"
                ));
            }
        }
        Err(error) => report.errors.push(error),
    }
    report
}

/// Push `routine`'s definition as it is now to `machine`, with the launch
/// recipe last prepared there (SW-XS-25). An edit reaches the machine with no
/// preparation, and the machine keeps starting the agent from what was
/// prepared. Content drift (its agent changed since, or nothing was prepared
/// there yet) is marked `content_stale`. Never prepares.
#[allow(clippy::too_many_arguments)]
fn push_definition(
    host: &dyn ArmHost,
    env: &dyn ArmEnv,
    input: &SyncInput<'_>,
    routine: &DiscoveredRoutine,
    machine: &str,
    agent: &AgentFacts,
    enforced: bool,
    held: Option<&ArmInfo>,
    report: &mut SyncReport,
) {
    let key = routine.key.to_string();
    let epoch = routine.def.arm_epoch.max(1);
    let place = match env.place(routine, machine) {
        Ok(place) => place,
        Err(error) => {
            report.unprepared.push((key.clone(), error));
            // Nothing to wake into or start: keep what the machine holds.
            if held.is_some() {
                return;
            }
            ArmPlace {
                workspace_id: "unprepared".into(),
                handle: String::new(),
                cwd: "/".into(),
            }
        }
    };
    // A recipe is used only for the workspace and the agent it was prepared
    // for: another agent's launch never goes out under this one.
    let agent_key = routine
        .agent_key
        .as_ref()
        .map(|k| k.to_string())
        .unwrap_or_default();
    let (recipe, not_for_agent) = match prepared(input.store, &key, machine) {
        Ok(Some(recipe)) if recipe.workspace_id != place.workspace_id => (None, None),
        Ok(Some(recipe)) if recipe.agent_key.is_empty() || recipe.agent_key != agent_key => {
            if let Err(error) = drop_prepared(input.store, &key, machine) {
                report.errors.push(format!("{key}: {error}"));
            }
            let why = if recipe.agent_key.is_empty() {
                STALE_AGENT_UNKNOWN
            } else {
                STALE_OTHER_AGENT
            };
            (None, Some(why))
        }
        Ok(recipe) => (recipe, None),
        Err(error) => {
            report.errors.push(format!("{key}: {error}"));
            return;
        }
    };
    let carried = carried_of(input, routine);
    let build = |facts: &AgentFacts, launch: Option<ArmLaunch>| {
        desired_arm(
            routine,
            (input.mode_of)(routine),
            &place,
            input.principal.as_str(),
            input.host_id,
            &(input.author_of)(routine),
            facts,
            &carried,
            launch,
            enforced,
        )
    };
    let arm = match recipe {
        Some(recipe) => {
            // The definition now, on the content prepared there.
            let facts = AgentFacts {
                manifest_hash: recipe.manifest_hash.clone(),
                allow_wake: agent.allow_wake,
            };
            let drifted = recipe.manifest_hash != agent.manifest_hash;
            set_stale(
                input,
                report,
                &key,
                machine,
                drifted.then_some(STALE_DRIFTED),
            );
            build(&facts, Some(recipe.launch))
        }
        None if not_for_agent.is_some() => {
            // What was prepared is for another agent (or none it can name):
            // the definition goes without a launch, so a fresh fire is
            // `needs_preparation` until a refresh prepares this agent there.
            set_stale(input, report, &key, machine, not_for_agent);
            build(agent, None)
        }
        None => {
            // Prepared there before this Mac kept its recipes: a push without
            // one would take its launch away. Kept, and marked unless current.
            if let Some(info) = held.filter(|info| info.has_launch && info.epoch == epoch) {
                let current = build(agent, None).is_ok_and(|arm| {
                    arm.source_digest == info.source_digest
                        && info.workspace_id == place.workspace_id
                });
                set_stale(
                    input,
                    report,
                    &key,
                    machine,
                    (!current).then_some(STALE_DRIFTED),
                );
                return;
            }
            let mut arm = build(agent, None);
            let mut same_content = false;
            if let (Some(info), Ok(next)) = (
                held.filter(|info| info.has_launch && info.epoch < epoch),
                &mut arm,
            ) {
                // The worker reports the manifest that produced its launch.
                // Reuse its receipt only for that same prepared content; the
                // worker separately checks the routine inputs are unchanged.
                same_content =
                    !info.prepared_digest.is_empty() && info.prepared_digest == agent.manifest_hash;
                if same_content {
                    next.source_digest = info.source_digest.clone();
                }
            }
            set_stale(
                input,
                report,
                &key,
                machine,
                Some(if same_content {
                    STALE_DRIFTED
                } else {
                    STALE_UNPREPARED
                }),
            );
            arm
        }
    };
    let arm = match arm {
        Ok(arm) => arm,
        Err(error) => {
            report.errors.push(format!("{key}: {error}"));
            return;
        }
    };
    // Already held from this very source, in this workspace, by this desktop.
    if held.is_some_and(|info| {
        info.epoch == arm.epoch
            && info.revision == arm.revision
            && info.workspace_id == place.workspace_id
            && info.source_digest == arm.source_digest
            && info.has_launch == arm.launch.is_some()
            && host
                .desktop_identity()
                .is_none_or(|me| info.originator.desktop == me)
    }) {
        return;
    }
    match host.arm(arm) {
        Ok(ArmAnswer::Armed { .. }) => report.armed.push(key),
        Ok(ArmAnswer::Refused {
            refusal: ArmRefusal::Stale { .. },
        }) => report.stale.push(key),
        Ok(other) => report.errors.push(format!("{key}: {other:?}")),
        Err(error) => report.errors.push(format!("{key}: {error}")),
    }
}

/// Mark `key`'s arm on `machine` stale for `why`, or clear it (`None`).
fn set_stale(
    input: &SyncInput<'_>,
    report: &mut SyncReport,
    key: &str,
    machine: &str,
    why: Option<&str>,
) {
    let done = match why {
        Some(why) => {
            report.content_stale.push(key.to_string());
            mark_stale(input.store, key, machine, why, input.now)
        }
        None => clear_stale(input.store, key, machine),
    };
    if let Err(error) = done {
        report.errors.push(format!("{key}: {error}"));
    }
}

/// Whether this Mac pushes `routine` to `machine`: armed there, managed here,
/// on, and not handing over (here, or as its file says to any Mac).
fn armable_here(
    input: &SyncInput<'_>,
    routine: &DiscoveredRoutine,
    machine: &str,
    still_pending: &HashSet<String>,
) -> bool {
    let def = &routine.def;
    def.armed_machine() == Some(machine)
        && super::host::manages_machine_arm(def, input.host_id)
        && !routine.held()
        && enabled(input, routine)
        && !still_pending.contains(&routine.key.to_string())
        && def
            .handover_from
            .as_ref()
            .is_none_or(|from| from.machine == machine)
}

/// Why a routine moved to a machine is not prepared there yet: the machine it
/// left has not confirmed it stopped firing it (SW-XS-25).
pub(crate) fn waiting_for(from: &str) -> String {
    format!("waiting for {from} to confirm it stopped firing it; refresh once it has")
}

/// Prepare `key`'s agent without holding the app's sync guard. Only explicit
/// actions prepare. The result is published separately after fresh discovery
/// under that guard, including fallback to the existing recipe on failure.
pub(crate) fn prepare_refresh(
    env: &dyn ArmEnv,
    preparer: &dyn ArmPrepare,
    input: &SyncInput<'_>,
    key: &str,
) -> Result<PreparedRefresh, String> {
    let routine = input
        .routines
        .iter()
        .find(|r| r.key.to_string() == key)
        .ok_or_else(|| format!("no routine {key} on this Mac"))?;
    let machine = routine
        .def
        .armed_machine()
        .ok_or_else(|| format!("routine {key} is not armed on a machine"))?
        .to_string();
    if !super::host::manages_machine_arm(&routine.def, input.host_id) {
        return Err(format!(
            "routine {key} is managed by another Mac: refresh it there"
        ));
    }
    let stale = |why: String| {
        if let Err(error) = mark_stale(input.store, key, &machine, &why, input.now) {
            log::warn!(target: "routines", "{key}: content_stale not recorded: {error}");
        }
        why
    };
    let pending =
        handovers(input.store).map_err(|e| format!("routine handovers unreadable: {e}"))?;
    // Moved here from another machine that has not let go: it is armed (and
    // can be prepared) here once that machine confirms.
    let waiting_on = pending
        .iter()
        .find(|h| h.routine_key == key && h.machine != machine)
        .map(|h| h.machine.clone())
        .or_else(|| {
            routine
                .def
                .handover_from
                .as_ref()
                .filter(|from| from.machine != machine)
                .map(|from| from.machine.clone())
        });
    if let Some(from) = waiting_on {
        return Err(stale(waiting_for(&from)));
    }
    let pending: HashSet<String> = pending.into_iter().map(|h| h.routine_key).collect();
    if !armable_here(input, routine, &machine, &pending) {
        return Err(format!(
            "routine {key} is paused or unassigned: nothing to refresh on {machine}"
        ));
    }
    let host = match env.host(&machine) {
        ArmLookup::Ready(host) => host,
        ArmLookup::Offline => return Err(stale(format!("{machine} is not connected"))),
        ArmLookup::Unknown => return Err(stale(format!("{machine} is not one of your machines"))),
        ArmLookup::Unsupported(why) => return Err(stale(why)),
    };
    let enforced = host.enforces_wake_limits();
    if !enforced
        && routine.def.policy.has_wake_limits()
        && (input.mode_of)(routine) == RoutineMode::Wake
    {
        return Err(stale(format!(
            "{machine}'s terminal server is too old to enforce this routine's wake limits"
        )));
    }
    let agent = (input.agent_of)(routine)
        .ok_or_else(|| stale("its agent cannot be read on this Mac".into()))?;
    let prepared_now = env.place(routine, &machine).and_then(|place| {
        if place.handle.is_empty() {
            return Err(format!("its workspace on {machine} is not open"));
        }
        let launch = preparer.prepare(routine, &machine, &place)?;
        Ok((place, launch))
    });
    Ok(PreparedRefresh {
        routine: routine.clone(),
        agent,
        machine,
        prepared_now,
    })
}

/// One explicit preparation, not yet published to the worker or local receipt.
/// Its input snapshot is checked again under the app's sync guard.
pub(crate) struct PreparedRefresh {
    routine: DiscoveredRoutine,
    agent: AgentFacts,
    machine: String,
    prepared_now: Result<(ArmPlace, ArmLaunch), String>,
}

/// Publish after fresh discovery under the sync guard: arm acceptance and
/// its saved receipt must stay ordered with every other refresh and sync.
pub(crate) fn publish_refresh(
    env: &dyn ArmEnv,
    input: &SyncInput<'_>,
    prepared: PreparedRefresh,
) -> Result<String, String> {
    let key = prepared.routine.key.to_string();
    let routine = input
        .routines
        .iter()
        .find(|r| r.key == prepared.routine.key)
        .ok_or_else(|| {
            format!("routine {key} changed while preparing; its preparation was discarded")
        })?;
    if routine.def != prepared.routine.def
        || routine.agent_key != prepared.routine.agent_key
        || (input.agent_of)(routine).as_ref() != Some(&prepared.agent)
    {
        return Err(format!(
            "routine {key} changed while preparing; its preparation was discarded"
        ));
    }
    let PreparedRefresh {
        agent,
        machine,
        prepared_now,
        ..
    } = prepared;
    let stale = |why: String| {
        if let Err(error) = mark_stale(input.store, &key, &machine, &why, input.now) {
            log::warn!(target: "routines", "{key}: content_stale not recorded: {error}");
        }
        why
    };
    let pending: HashSet<_> = handovers(input.store)
        .map_err(|e| format!("routine handovers unreadable: {e}"))?
        .into_iter()
        .map(|h| h.routine_key)
        .collect();
    if !armable_here(input, routine, &machine, &pending) {
        return Err(format!(
            "routine {key} is no longer ready to refresh on {machine}"
        ));
    }
    let host = match env.host(&machine) {
        ArmLookup::Ready(host) => host,
        ArmLookup::Offline => return Err(stale(format!("{machine} is not connected"))),
        ArmLookup::Unknown => return Err(stale(format!("{machine} is not one of your machines"))),
        ArmLookup::Unsupported(why) => return Err(stale(why)),
    };
    let enforced = host.enforces_wake_limits();
    let held_now = host.arms().map_err(&stale)?;
    let held = held_now.armed.iter().find(|info| info.key == key);
    let (place, launch) = match prepared_now {
        Ok(prepared) => prepared,
        Err(why) => {
            // Not prepared now: its definition still goes, on what was
            // prepared there before.
            let mut report = SyncReport::default();
            push_definition(
                host.as_ref(),
                env,
                input,
                routine,
                &machine,
                &agent,
                enforced,
                held,
                &mut report,
            );
            if !report.content_stale.is_empty() {
                stale(why.clone());
            }
            return Err(why);
        }
    };
    let arm = desired_arm(
        routine,
        (input.mode_of)(routine),
        &place,
        input.principal.as_str(),
        input.host_id,
        &(input.author_of)(routine),
        &agent,
        &carried_of(input, routine),
        Some(launch.clone()),
        enforced,
    )
    .map_err(&stale)?;
    match host.arm(arm) {
        Ok(ArmAnswer::Armed { .. }) => {
            save_prepared(
                input.store,
                &key,
                &machine,
                &Prepared {
                    workspace_id: place.workspace_id,
                    agent_key: routine
                        .agent_key
                        .as_ref()
                        .map(|k| k.to_string())
                        .unwrap_or_default(),
                    manifest_hash: agent.manifest_hash,
                    launch,
                },
                input.now,
            )
            .map_err(|e| e.to_string())?;
            clear_stale(input.store, &key, &machine).map_err(|e| e.to_string())?;
            Ok(machine)
        }
        Ok(ArmAnswer::Refused {
            refusal: ArmRefusal::Stale { .. },
        }) => Err(stale(format!(
            "{machine} holds a newer arm of it (it was moved from another Mac)"
        ))),
        Ok(other) => Err(stale(format!("{machine}: {other:?}"))),
        Err(error) => Err(stale(error)),
    }
}

/// What this Mac carries of `routine` to its machine: the last occurrence
/// it fired, and its last summary and run time.
fn carried_of(input: &SyncInput<'_>, routine: &DiscoveredRoutine) -> Carried {
    let key = routine.key.to_string();
    let state = input.store.state(&routine.key).ok().flatten();
    let last_fired: Option<i64> = input
        .store
        .transaction(|tx| {
            Ok(tx
                .query_row(
                    "SELECT MAX(created_at) FROM routine_runs
                      WHERE routine_key = ?1 AND source IN ('schedule', 'catch_up')
                        AND occurrence_key NOT LIKE '%@machine:%'",
                    params![key],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .optional()?
                .flatten())
        })
        .ok()
        .flatten();
    Carried {
        last_fired_ms: last_fired.map(|ms| ms as u64),
        last_run_summary: state.as_ref().and_then(|s| s.last_result_text.clone()),
        last_run_at_ms: state.and_then(|s| s.last_run_at).map(|ms| ms as u64),
        wake_budget: input.store.wake_budget(&key).unwrap_or(0),
    }
}

fn enabled(input: &SyncInput<'_>, routine: &DiscoveredRoutine) -> bool {
    input
        .store
        .is_enabled(&routine.key, &routine.def, input.host_id)
        .unwrap_or(false)
}

/// Read `machine`'s run log for routines this Mac currently manages. The
/// worker's epoch fences a Mac whose local definition has not pulled a move;
/// earlier run epochs remain legitimate history for the current manager.
fn record_runs(
    host: &dyn ArmHost,
    input: &SyncInput<'_>,
    machine: &str,
) -> Result<(usize, bool), String> {
    let since = cursor(input.store, machine).map_err(|e| e.to_string())?;
    let log = host.runs(since)?;
    // A replaced log starts from zero on the next pass.
    let mut next = if log.last_seq < since {
        0
    } else {
        log.last_seq
    };
    let arms = host.arms()?;
    let mut by_key: HashMap<_, _> = input
        .routines
        .iter()
        .map(|routine| {
            (
                routine.key.to_string(),
                super::host::names_mac(&routine.def, input.host_id)
                    .then_some(routine.def.arm_epoch.max(1)),
            )
        })
        .collect();
    let mut recorded = 0;
    for run in &log.runs {
        // Firing discovery excludes archives; pending outcomes still belong
        // to their current owner. Resolve each missing definition once.
        let Some(epoch) = *by_key.entry(run.key.clone()).or_insert_with(|| {
            let def = (input.undiscovered)(&run.key)?;
            super::host::names_mac(&def, input.host_id).then_some(def.arm_epoch.max(1))
        }) else {
            continue;
        };
        let held = arms.armed.iter().find(|arm| arm.key == run.key);
        let tomb = arms.disarmed.iter().find(|tomb| tomb.key == run.key);
        let acknowledged = held.map(|arm| arm.epoch).or(tomb.map(|tomb| tomb.epoch)) == Some(epoch);
        if run.epoch > epoch || !acknowledged {
            // Do not consume history before the worker acknowledges this
            // owner. Retain only its log position, never a buffered run.
            next = next.min(run.seq.saturating_sub(1));
            continue;
        }
        if record_run(input.store, machine, run, input.now).map_err(|e| e.to_string())? {
            recorded += 1;
            // Earlier epochs are history, not a new pause or notification.
            if run.epoch != epoch {
                continue;
            }
            // SW-AGR-13: fired on the machine (the desktop may have been
            // closed): a run that ended on a wake limit pauses the routine
            // or says so, as a desktop-fired one does.
            if let ArmRunStatus::Undelivered { reason, detail } = &run.status {
                super::wake::on_limit(
                    input.store,
                    &super::wake::LimitHit {
                        run_id: &run.run_id,
                        fired_at: i64::try_from(run.fired_at_ms).unwrap_or(i64::MAX),
                        routine_key: &run.key,
                        target: &crate::delivery::authority::SessionRef::new(machine, ""),
                        reason: *reason,
                        notice: detail.as_deref(),
                    },
                    input.notify,
                );
            }
        }
    }
    // Runs it fired were dropped before this Mac read them: say so (the
    // routine's view shows it), instead of a silent gap.
    let lost = log.overflow.then_some(input.now);
    if lost.is_some() {
        log::warn!(target: "routines", "machine {machine}: routine runs after {since} were dropped from its log before this Mac read them");
    }
    save_cursor(input.store, machine, next, &log.host_boot_id, lost).map_err(|e| e.to_string())?;
    Ok((recorded, lost.is_some()))
}

/// The run row a machine run's status stands for: phase, reason, and the
/// detail kept as its text.
fn row_of(status: &ArmRunStatus) -> (&'static str, Option<String>, Option<String>) {
    use crate::delivery::run_status::reason_wire;
    match status {
        ArmRunStatus::Firing => (phase::QUEUED, Some(REASON_ON_MACHINE.into()), None),
        ArmRunStatus::Queued { .. } => (phase::QUEUED, Some(REASON_ON_MACHINE.into()), None),
        ArmRunStatus::Delivered { .. } | ArmRunStatus::Launched { .. } => {
            (phase::RUNNING, None, None)
        }
        ArmRunStatus::Coalesced { into } => (
            phase::COALESCED,
            None,
            Some(format!("coalesced into run {into}")),
        ),
        ArmRunStatus::Skipped { why } => {
            (phase::SKIPPED, Some("machine".into()), Some(why.clone()))
        }
        ArmRunStatus::Undelivered { reason, detail } => (
            phase::UNDELIVERED,
            Some(reason_wire(*reason).to_string()),
            detail.clone(),
        ),
        ArmRunStatus::Unknown { detail } => (phase::UNKNOWN, None, Some(detail.clone())),
        ArmRunStatus::TimedOut { .. } => (
            phase::TIMED_OUT,
            None,
            Some("passed its max_runtime on the machine, which stopped it".into()),
        ),
        ArmRunStatus::Exited { .. } => (
            phase::ENDED,
            None,
            Some("its session on the machine ended without a verdict".into()),
        ),
        ArmRunStatus::Unrecognized => (
            phase::UNKNOWN,
            None,
            Some("the machine reported a status this app does not know".into()),
        ),
        // The agent finished it on the machine, with no desktop to tell.
        ArmRunStatus::Done {
            outcome, summary, ..
        } => (
            match outcome.as_str() {
                "success" => phase::SUCCEEDED,
                "needs-attention" => phase::NEEDS_YOU,
                _ => phase::FAILED,
            },
            None,
            Some(summary.clone()).filter(|s| !s.is_empty()),
        ),
    }
}

/// The reason a machine run that is not settled yet carries: it is the
/// machine's, never this Mac's claim loop's.
pub(crate) const REASON_ON_MACHINE: &str = "on_machine";

/// Record one machine run log entry as its run row. A row only moves forward:
/// a settled row (one this Mac finished, or the machine did) is never moved
/// back by a re-read. True when a row changed.
pub(crate) fn record_run(
    store: &RoutineStateStore,
    machine: &str,
    run: &ArmRun,
    now: i64,
) -> StoreResult<bool> {
    let (phase, reason, text) = row_of(&run.status);
    let (session, delivered, launched) = match &run.status {
        ArmRunStatus::Delivered { session, at_ms } => (Some(session), Some(*at_ms as i64), false),
        ArmRunStatus::Launched { session, at_ms } => (
            Some(session),
            (run.mode == ArmMode::Wake).then_some(*at_ms as i64),
            true,
        ),
        ArmRunStatus::Queued { session, .. } => (Some(session), None, false),
        _ => (None, None, false),
    };
    let ended = run.status.is_final() && phase != super::runs::phase::RUNNING;
    // A scheduled occurrence once per epoch; a run by hand is its own.
    let occurrence = if run.source == "manual" {
        format!("{}@machine:{machine}@manual@{}", run.key, run.run_id)
    } else {
        format!(
            "{}@machine:{machine}@{}@{}",
            run.key, run.epoch, run.scheduled_at_ms
        )
    };
    let changed = store.transaction(|tx| {
        let existing: Option<String> = tx
            .query_row(
                "SELECT phase FROM routine_runs WHERE run_id = ?1",
                params![run.run_id],
                |r| r.get(0),
            )
            .optional()?;
        let rank = |p: &str| match p {
            "queued" => 0,
            "running" => 1,
            _ => 2,
        };
        // The agent's own verdict replaces an ending without one (this Mac
        // may have seen its session end before reading the verdict).
        let verdict = matches!(run.status, ArmRunStatus::Done { .. });
        if let Some(current) = &existing {
            let settled_without_verdict = current == phase::ENDED && verdict;
            if !settled_without_verdict
                && (rank(current) > rank(phase) || (rank(current) == 2 && current != phase))
            {
                return Ok(false);
            }
        }
        let session = session.map(|s| s.as_str().to_string());
        if existing.is_none() {
            tx.execute(
                "INSERT INTO routine_runs (run_id, routine_key, revision, source, occurrence_key,
                     phase, reason, session_id, created_at, started_at, ended_at, result_text,
                     delivered_at, wake_machine, launch_machine)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    run.run_id,
                    run.key,
                    run.revision,
                    run.source,
                    occurrence,
                    phase,
                    reason,
                    session,
                    run.fired_at_ms as i64,
                    (phase == super::runs::phase::RUNNING).then_some(run.at_ms as i64),
                    ended.then_some(now),
                    text,
                    delivered,
                    (!launched && session.is_some()).then_some(machine),
                    launched.then_some(machine),
                ],
            )?;
            return Ok(true);
        }
        let changed = tx.execute(
            "UPDATE routine_runs SET phase = ?2, reason = ?3,
                    session_id = COALESCE(?4, session_id),
                    started_at = COALESCE(started_at, ?5),
                    ended_at = COALESCE(ended_at, ?6),
                    result_text = COALESCE(?7, result_text),
                    delivered_at = COALESCE(delivered_at, ?8),
                    wake_machine = COALESCE(wake_machine, ?9),
                    launch_machine = COALESCE(launch_machine, ?10)
              WHERE run_id = ?1 AND (phase IN ('queued', 'running') OR (phase = 'ended' AND ?11))",
            params![
                run.run_id,
                phase,
                reason,
                session,
                (phase == super::runs::phase::RUNNING).then_some(run.at_ms as i64),
                ended.then_some(now),
                text,
                delivered,
                (!launched && session.is_some()).then_some(machine),
                launched.then_some(machine),
                verdict,
            ],
        )?;
        Ok(changed > 0)
    })?;
    if changed {
        super::events::changed(&run.key, &run.run_id);
    }
    Ok(changed)
}

/// The machines a sync pass visits: every machine a discovered routine is
/// armed on, and every machine a handover waits on.
pub(crate) fn machines_to_sync(
    routines: &[DiscoveredRoutine],
    handovers: &[Handover],
) -> Vec<String> {
    let mut machines: Vec<String> = routines
        .iter()
        .filter_map(|routine| routine.def.armed_machine().map(str::to_string))
        .chain(handovers.iter().map(|h| h.machine.clone()))
        .collect();
    machines.sort();
    machines.dedup();
    machines
}

/// How many prepared paths a recipe carries for the machine to check.
const PREPARED_PATH_CAP: usize = 256;

/// The launch recipe for a routine's agent `agent`, prepared into `place`:
/// its native launch inputs (no first prompt: each fire brings its own), the
/// paths the preparation wrote (the machine checks they are still there), and
/// the tab it opens in, tagged as a desktop launch of that agent would be.
pub(crate) fn launch_recipe(
    agent: &crate::agents::remote_launch::CapturedAgent,
    place: &ArmPlace,
    owner: &str,
    team_key: &str,
    workspace: codesense_pty_core::domain::WorkspaceKey,
    descriptor: Option<codesense_pty_core::layout::WorkspaceDescriptor>,
) -> ArmLaunch {
    let mut startup = agent.startup.clone();
    startup.prompt = None;
    let mut prepared_paths: Vec<String> = agent
        .manifest
        .entries
        .iter()
        .flat_map(|entry| entry.files.iter().map(|file| file.destination.clone()))
        .collect();
    prepared_paths.sort();
    prepared_paths.dedup();
    prepared_paths.truncate(PREPARED_PATH_CAP);
    let mut tags = codesense_pty_core::layout::Tags::new();
    tags.insert("remoteAgentKey".into(), agent.key());
    tags.insert("remoteWorkspaceId".into(), place.workspace_id.clone());
    if !owner.is_empty() {
        tags.insert(super::wake_app::TAG_REMOTE_OWNER.into(), owner.to_string());
    }
    tags.insert(
        "remoteLaunch".into(),
        serde_json::to_string(&agent.runtime).unwrap_or_default(),
    );
    tags.insert("remotePreparedWorkspace".into(), place.handle.clone());
    // Its team, so a desktop relaying its CodeSense MCP knows it.
    tags.insert(
        codesense_pty_core::routine_arm::TAG_ROUTINE_TEAM.into(),
        team_key.to_string(),
    );
    tags.insert(
        "remoteStartupManifest".into(),
        agent.manifest.manifest_hash.clone(),
    );
    ArmLaunch {
        launch: NativeLaunch::Cli(agent.runtime),
        agent_startup: startup,
        prepared_paths,
        prepared_digest: agent.manifest.manifest_hash.clone(),
        workspace,
        descriptor,
        tags,
    }
}
