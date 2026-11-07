// SPDX-License-Identifier: MIT
//! `RoutineArmV1`: the wire of persistent routines armed on a machine.
//!
//! A routine whose definition names a machine (`armed_machine`) is armed on
//! that machine's terminal server: the server keeps the routine's schedule,
//! evaluates it on its own clock in the routine's IANA zone (the tzdb
//! compiled into the server), and fires it whether or not any desktop is
//! connected.
//!
//! - A **wake** fire is delivered through the server's own `SessionWakeV1`
//!   queue into a live session of the routine's agent (idle-gated, never
//!   typed into a busy or needs-you session). With no such session, it starts
//!   one, but only like a fresh fire does.
//! - A **fresh** fire starts the agent only into a workspace a desktop already
//!   prepared, from the launch recipe the desktop pushed with the arm. With no
//!   recipe, or a workspace that is no longer there, the run is
//!   `undelivered(needs_preparation)` until a desktop prepares it again.
//!
//! **Fencing.** Every arm carries the routine's *arm epoch* from git, bumped
//! on every move between a desktop and a machine. The server keeps the
//! highest epoch it has seen for a routine, armed or disarmed (a tombstone),
//! and refuses an arm or a disarm older than it. It re-checks the epoch before
//! every fire, so a stale definition can never re-arm a routine that was
//! moved. A disarm without a move (paused, archived) leaves a tombstone at
//! the routine's own epoch, which that same placement may re-arm.
//!
//! **Runs** are written to a capped log the desktop reads with a cursor when
//! it (re)connects, like the wake outcome log. A run's id is derived from the
//! routine, its epoch and its scheduled instant, so a restart never fires the
//! same occurrence twice under two ids.
//!
//! Every time on this wire is a host-clock instant the host reported itself,
//! or a duration; a desktop's wall clock never crosses it.

use std::collections::BTreeMap;

use codesense_host_proto::agent_cli::NativeLaunch;
use codesense_host_proto::reverse_mcp::AgentStartup;
use serde::{Deserialize, Serialize};

use crate::domain::PtySessionId;
use crate::domain::WorkspaceKey;
use crate::layout::{Tags, WorkspaceDescriptor};
use crate::session_wake::{Originator, UndeliveredReason, WakeLimits};

/// How many routines one machine may hold armed.
pub const MACHINE_ARM_CAP: usize = 200;
/// How many schedules one armed routine may have.
pub const ARM_SCHEDULE_CAP: usize = 8;
/// The shortest interval between two fires of one schedule.
pub const MIN_ARM_INTERVAL_MS: u64 = 60 * 1000;
/// How many runs the host keeps for desktops that were away.
pub const ARM_RUN_LOG_CAP: usize = 1000;
/// A fire whose instant passed longer ago than this was missed (the machine
/// was down or asleep), not merely late.
pub const MISSED_GRACE_MS: u64 = 2 * 60 * 1000;
/// The largest task text, in bytes.
pub const MAX_TASK_BYTES: usize = 8 * 1024;
/// How long a fresh run may run when its arm names no `max_runtime` (the
/// longest a routine may declare).
pub const DEFAULT_MAX_RUNTIME_MS: u64 = 24 * 60 * 60 * 1000;
/// The workspace an arm names before any desktop found the routine's
/// workspace on the machine: nothing is woken or started there.
pub const UNPREPARED_WORKSPACE: &str = "unprepared";
/// The tab tag a launch-if-absent carries (a routine's wake that found no
/// session of its agent): the host starts it only when no session of that
/// agent runs, or is being started, in that workspace (one flight per agent
/// and workspace, shared by every launcher).
pub const TAG_LAUNCH_IF_ABSENT: &str = "remoteLaunchIfAbsent";
/// The code (and the error text's prefix) a launch-if-absent is refused with
/// when the agent already runs there, or is being started: deliver to it.
pub const AGENT_RUNNING: &str = "agentRunning";
/// The tab tag naming the session a machine-armed routine started.
pub const TAG_ROUTINE_RUN_SESSION: &str = "routineRunSession";
/// The tab tag carrying the team (`org:team`) of the routine a machine
/// started an agent for, so a desktop binding its CodeSense MCP knows it.
pub const TAG_ROUTINE_TEAM: &str = "routineTeamKey";

/// What a fire does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmMode {
    /// Deliver the task into the agent's live session; start one if absent.
    Wake,
    /// Start a new instance of the agent with the task as its first prompt.
    Fresh,
}

/// What a fresh fire does while the last one it started still runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmOverlap {
    /// Record the fire as coalesced into the run still going.
    #[default]
    Coalesce,
    /// Record the fire as skipped.
    Skip,
}

/// What a fire whose instant passed while the machine was down does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmMissed {
    /// Record one skipped run for the missed occurrences, fire nothing.
    #[default]
    Skip,
    /// Fire once for the missed occurrences.
    RunOnce,
}

/// One schedule: a 5-field cron in an IANA zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmSchedule {
    pub cron: String,
    pub tz: String,
}

/// Which sessions a wake may deliver into: the routine's saved agent, in the
/// routine's workspace on this machine, launched by the routine's owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmTarget {
    /// The saved agent's key, without a revision.
    pub agent: String,
    /// The machine workspace the routine runs in (its `remoteWorkspaceId`).
    pub workspace_id: String,
    /// Its directory on this machine.
    pub cwd: String,
    /// The account that launched the sessions a wake may type into (the
    /// `remoteOwnerId` tab tag). A tab tagged with another is never a
    /// candidate; an untagged tab counts as this owner's (the private
    /// terminal server admits only its enrolled owner).
    #[serde(default)]
    pub owner: Option<String>,
    /// The agent set `allowWake: true`: a session of it that runs without
    /// permission checks may be woken. Without it, such a session (a raw
    /// `bypass` launch) is never a candidate.
    #[serde(default)]
    pub allow_wake: bool,
}

/// How this machine starts the agent, prepared by a desktop: the saved
/// agent's native launch inputs, the workspace they were prepared into, and
/// the tab it opens in. The first prompt is filled in per fire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmLaunch {
    pub launch: NativeLaunch,
    /// The saved agent's args and settings; its `prompt` is ignored.
    pub agent_startup: AgentStartup,
    /// Paths (relative to the workspace) the desktop's preparation wrote.
    /// Every one must still exist for the workspace to count as prepared.
    #[serde(default)]
    pub prepared_paths: Vec<String>,
    /// A digest of what was prepared, for the desktop to compare.
    #[serde(default)]
    pub prepared_digest: String,
    /// The layout group the tab opens in.
    pub workspace: WorkspaceKey,
    #[serde(default)]
    pub descriptor: Option<WorkspaceDescriptor>,
    /// The tab's tags (`remoteAgentKey`, `remoteWorkspaceId`, …).
    #[serde(default)]
    pub tags: Tags,
}

/// A routine to arm (or re-arm, after an edit) on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineArm {
    /// The routine key (`<org>/<team>/<id>` or a personal key).
    pub key: String,
    /// The routine's arm epoch from git (at least 1).
    pub epoch: u64,
    /// The pinned routine revision this arm carries.
    pub revision: u32,
    pub name: String,
    pub mode: ArmMode,
    pub schedules: Vec<ArmSchedule>,
    /// The task, with only `{{date}}` and `{{timestamp}}` rendered per fire.
    pub task: String,
    pub target: ArmTarget,
    /// How long a wake may wait for its session to be idle.
    pub max_wait_ms: u64,
    #[serde(default)]
    pub overlap: ArmOverlap,
    #[serde(default)]
    pub missed: ArmMissed,
    /// Present once a desktop prepared the workspace and the agent.
    #[serde(default)]
    pub launch: Option<ArmLaunch>,
    /// The routine host (desktop install) that manages this arm and records
    /// its runs. The host stamps `desktop` itself from the connection.
    #[serde(default)]
    pub originator: Originator,
    /// Who last edited the routine, for the delivery marker.
    #[serde(default)]
    pub author: String,
    /// How long a fresh run may run before it is stopped (`max_runtime`);
    /// 0 is [`DEFAULT_MAX_RUNTIME_MS`].
    #[serde(default)]
    pub max_runtime_ms: u64,
    /// The last occurrence the routine fired where it was armed before
    /// (host clock of the desktop that pushed it): the machine never fires
    /// an occurrence at or before it.
    #[serde(default)]
    pub last_fired_ms: Option<u64>,
    /// `{{last_run_summary}}` and `{{last_run_at}}` until this machine has
    /// its own (the desktop's, from the runs it fired).
    #[serde(default)]
    pub last_run_summary: Option<String>,
    #[serde(default)]
    pub last_run_at_ms: Option<u64>,
    /// A digest of everything the desktop built this arm from (the
    /// definition, its agent's manifest and posture), echoed in [`ArmInfo`]:
    /// an arm whose source changed is pushed again.
    #[serde(default)]
    pub source_digest: String,
    /// The routine's wake limits, carried into every delivery a
    /// wake fire makes on this machine's queue, which enforces them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<WakeLimits>,
}

/// An armed routine, as the host holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmInfo {
    pub key: String,
    pub epoch: u64,
    pub revision: u32,
    pub mode: ArmMode,
    /// Host clock of the next fire, if any schedule still fires.
    pub next_at_ms: Option<u64>,
    pub armed_at_ms: u64,
    /// Whether a launch recipe is held (fresh fires can start the agent).
    pub has_launch: bool,
    #[serde(default)]
    pub prepared_digest: String,
    /// The machine workspace a wake looks in (and a fresh fire starts in).
    #[serde(default)]
    pub workspace_id: String,
    pub originator: Originator,
    /// The arm's [`RoutineArm::source_digest`].
    #[serde(default)]
    pub source_digest: String,
    /// The last occurrence this machine fired.
    #[serde(default)]
    pub last_fired_ms: Option<u64>,
}

/// A routine the host no longer fires, and the epoch that disarmed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmTombstone {
    pub key: String,
    pub epoch: u64,
    /// The last occurrence it fired here: wherever it fires next starts
    /// after it.
    #[serde(default)]
    pub last_fired_ms: Option<u64>,
}

/// Everything the host holds: armed routines and the tombstones of disarmed
/// ones (each with the highest epoch it has seen).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmList {
    pub armed: Vec<ArmInfo>,
    pub disarmed: Vec<ArmTombstone>,
    /// Why this host cannot run routines without a desktop, when it cannot
    /// (it refuses every arm with this reason).
    #[serde(default)]
    pub unsupported: Option<String>,
}

/// Why an arm or a disarm was refused. Nothing changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArmRefusal {
    /// The host already holds a newer epoch (or, at the same epoch, a newer
    /// revision) of this routine.
    Stale { held_epoch: u64, held_revision: u32 },
    /// The request is malformed (cron, zone, caps, labels).
    Invalid { detail: String },
    /// The arm store cannot be written.
    Unavailable { detail: String },
}

impl std::fmt::Display for ArmRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stale {
                held_epoch,
                held_revision,
            } => write!(
                f,
                "the machine already holds a newer arming of this routine (epoch {held_epoch}, revision {held_revision})"
            ),
            Self::Invalid { detail } => f.write_str(detail),
            Self::Unavailable { detail } => {
                write!(f, "the machine cannot store routine arms: {detail}")
            }
        }
    }
}

/// The answer to an arm or a disarm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ArmAnswer {
    Armed {
        info: ArmInfo,
    },
    Disarmed {
        was_armed: bool,
        /// The last occurrence it fired here.
        #[serde(default)]
        last_fired_ms: Option<u64>,
    },
    Refused {
        refusal: ArmRefusal,
    },
}

/// The answer to firing an armed routine now, by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ArmFireAnswer {
    /// Fired through the routine's policy: the run as it stands.
    Fired {
        run: Box<ArmRun>,
    },
    Refused {
        refusal: ArmRefusal,
    },
}

/// The answer to finishing a run this host fired (`routine_done` or
/// `signal_done` relayed by a desktop).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ArmCompleteAnswer {
    /// Finished: its `done` entry.
    Done { run: Box<ArmRun> },
    /// Not finished, and why (not a run of this host in that session, or
    /// already finished).
    Refused { detail: String },
}

/// Where a machine-fired run stands. `firing` and `queued` are not final.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ArmRunStatus {
    /// Claimed: its cursor moved, the delivery or launch is being made.
    Firing,
    /// A wake waits in the session's queue until its agent is idle.
    Queued {
        session: PtySessionId,
        deadline_ms: u64,
    },
    /// A wake was written into the session (bracketed paste, one submit).
    Delivered { session: PtySessionId, at_ms: u64 },
    /// The agent was started with the task as its first prompt.
    Launched { session: PtySessionId, at_ms: u64 },
    /// A fresh fire while the last one still runs (`overlap: coalesce`).
    Coalesced { into: String },
    /// Not fired: `overlap: skip`, or occurrences missed while the machine
    /// was down (`missed: skip`).
    Skipped { why: String },
    Undelivered {
        reason: UndeliveredReason,
        #[serde(default)]
        detail: Option<String>,
    },
    /// A write or a launch began but was not confirmed: it may have reached
    /// the session or started the agent. Never retried (R5).
    Unknown { detail: String },
    /// The agent finished the run itself (`routine_done`, or `signal_done`
    /// for the run it was started or woken for), answered by the machine
    /// while no desktop relayed its CodeSense MCP. `outcome` is `success`,
    /// `failed` or `needs-attention`.
    Done {
        outcome: String,
        #[serde(default)]
        summary: String,
        at_ms: u64,
    },
    /// A fresh run passed its `max_runtime`: its session was stopped.
    TimedOut { at_ms: u64 },
    /// A fresh run's session ended without the agent finishing the run.
    Exited { at_ms: u64 },
    /// A status a newer host wrote that this build does not know: final.
    #[serde(other)]
    Unrecognized,
}

/// The outcomes an agent finishes a run with.
pub const RUN_OUTCOMES: [&str; 3] = ["success", "failed", "needs-attention"];

impl ArmRunStatus {
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::Firing | Self::Queued { .. })
    }
}

/// One run of a machine-armed routine. The log holds one entry per status
/// change; the newest entry of a `run_id` is its current status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmRun {
    /// The log position.
    pub seq: u64,
    pub run_id: String,
    pub key: String,
    pub epoch: u64,
    pub revision: u32,
    pub mode: ArmMode,
    /// `schedule`, or `catch_up` for one fire standing for missed ones.
    pub source: String,
    /// Host clock: the occurrence's instant, and when it fired.
    pub scheduled_at_ms: u64,
    pub fired_at_ms: u64,
    pub status: ArmRunStatus,
    pub originator: Originator,
    /// The host lifetime that wrote this entry.
    pub host_boot_id: String,
    /// Host clock of this entry.
    pub at_ms: u64,
}

/// The runs a desktop missed: those after `since`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmRunLog {
    pub host_boot_id: String,
    pub runs: Vec<ArmRun>,
    /// Some entries after `since` were dropped from the capped log.
    pub overflow: bool,
    /// The newest log position; pass it as the next `since`.
    pub last_seq: u64,
}

/// The id of the run an armed routine fires for the occurrence at
/// `scheduled_at_ms` under arm `epoch`: the same on every host lifetime, so
/// a restart can never fire one occurrence twice under two ids.
pub fn arm_run_id(key: &str, epoch: u64, scheduled_at_ms: u64) -> String {
    // A UUID v8 (custom) over a keyed BLAKE3 digest: stable, opaque,
    // uuid-formatted like every other run id.
    let digest = blake3::hash(
        format!("codesense-routine-arm\0{key}\0{epoch}\0{scheduled_at_ms}").as_bytes(),
    );
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest.as_bytes()[..16]);
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(b).to_string()
}

/// The reverse-MCP binding nonce of `session`, an agent a machine started
/// for a routine while a desktop that gave it `secret` was connected: the
/// desktop accepts that session's CodeSense MCP only with this nonce, so a
/// binding it never asked for is one this machine made for it.
pub fn routine_binding_nonce(secret: &str, session: &str) -> String {
    blake3::hash(format!("codesense-routine-binding\0{secret}\0{session}").as_bytes())
        .to_hex()
        .to_string()
}

/// The ASCII run header a machine-fired routine run starts with, the same
/// shape as a desktop-fired one: `[routine: <name> | run <id>]`.
pub fn routine_header(name: &str, run_id: &str) -> String {
    fn ascii(text: &str) -> String {
        text.chars()
            .map(|c| match c {
                ']' | '|' | '[' => '?',
                c if c.is_ascii() && !c.is_ascii_control() => c,
                _ => '?',
            })
            .collect::<String>()
            .trim()
            .to_string()
    }
    format!("[routine: {} | run {}]", ascii(name), ascii(run_id))
}

/// `task` with `{{date}}` and `{{timestamp}}` (and their spaced forms)
/// filled in. Nothing else is rendered: every other token stays literal, as
/// in a desktop wake.
pub fn render_static(task: &str, values: &BTreeMap<&str, String>) -> String {
    let mut out = String::with_capacity(task.len());
    let mut rest = task;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        match after.find("}}") {
            Some(close) => {
                let name = after[..close].trim();
                match values.get(name) {
                    Some(value) => out.push_str(value),
                    None => out.push_str(&rest[open..open + 2 + close + 2]),
                }
                rest = &after[close + 2..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_id_is_stable_per_occurrence_and_uuid_shaped() {
        let a = arm_run_id("o/t/r", 3, 1_790_000_000_000);
        assert_eq!(a, arm_run_id("o/t/r", 3, 1_790_000_000_000));
        assert_ne!(a, arm_run_id("o/t/r", 4, 1_790_000_000_000));
        assert_ne!(a, arm_run_id("o/t/r", 3, 1_790_000_060_000));
        let parsed = uuid::Uuid::parse_str(&a).unwrap();
        assert_eq!(parsed.get_version_num(), 8);
    }

    #[test]
    fn only_date_and_timestamp_render() {
        let values = BTreeMap::from([
            ("date", "2026-10-06".to_string()),
            ("timestamp", "2026-10-06T09:00:00Z".to_string()),
        ]);
        assert_eq!(
            render_static(
                "triage {{date}} at {{ timestamp }}; last {{last_run_summary}} {{",
                &values
            ),
            "triage 2026-10-06 at 2026-10-06T09:00:00Z; last {{last_run_summary}} {{"
        );
    }

    #[test]
    fn the_header_is_one_line_of_ascii() {
        assert_eq!(
            routine_header("Triage · [daily] | x", "r-1"),
            "[routine: Triage ? ?daily? ? x | run r-1]"
        );
    }

    #[test]
    fn statuses_round_trip_and_only_firing_and_queued_are_open() {
        let status = ArmRunStatus::Undelivered {
            reason: UndeliveredReason::NeedsPreparation,
            detail: Some("no launch recipe".into()),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["status"], "undelivered");
        assert_eq!(json["reason"], "needs_preparation");
        assert_eq!(
            serde_json::from_value::<ArmRunStatus>(json).unwrap(),
            status
        );
        assert!(!ArmRunStatus::Firing.is_final());
        assert!(!ArmRunStatus::Queued {
            session: PtySessionId::new("s"),
            deadline_ms: 1
        }
        .is_final());
        assert!(ArmRunStatus::Skipped { why: "x".into() }.is_final());
    }
}
